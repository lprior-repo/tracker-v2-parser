use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use futures::{TryStreamExt, stream};
use serde_json::{Value, json};
use track_scrapper_v2::{database::Database, durable::model::CrawlRequest, scope::LOWER_48};

#[path = "coverage_selector/changes.rs"]
mod changes;

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 100)]
    samples: usize,
    #[arg(long)]
    mutate_replica: bool,
    #[arg(long, default_value_t = 20.0)]
    max_selection_ms: f64,
    #[arg(long)]
    state: Option<String>,
    #[arg(long)]
    production_read_only: bool,
    #[arg(long)]
    diagnostics: bool,
}

async fn expected(database: &Database, request: &CrawlRequest, now: i64) -> Result<Option<i64>> {
    let scope = request.page_scope()?;
    Ok(
        sqlx::query_as::<_, (i64, Value)>(include_str!("../src/database/profile_coverage.sql"))
            .bind(Some("milesplit"))
            .bind(3_i32)
            .bind(&request.config.states)
            .bind(&request.config.graduation_years)
            .bind(false)
            .bind(LOWER_48.as_slice())
            .bind(matches!(
                request.config.acquisition,
                track_scrapper_v2::acquisition::AcquisitionPolicy::Complete
                    | track_scrapper_v2::acquisition::AcquisitionPolicy::PublicResults {
                        collect_contacts: true,
                        ..
                    }
            ))
            .bind(&scope.run_generation)
            .bind(now)
            .bind([0_i64; 0].as_slice())
            .bind(true)
            .fetch_optional(database.pool())
            .await?
            .map(|row| row.0),
    )
}

async fn measured(
    database: &Database,
    request: &CrawlRequest,
    now: i64,
) -> Result<(Option<i64>, f64)> {
    let scope = request.page_scope()?;
    let start = Instant::now();
    let selected = database
        .next_durable_page(&request.config, &scope, now)
        .await?
        .map(|job| job.id);
    Ok((selected, start.elapsed().as_secs_f64() * 1000.0))
}

fn statistics(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let length = samples.len();
    let percentile = |numerator: usize| {
        samples
            .get(
                length
                    .saturating_sub(1)
                    .saturating_mul(numerator)
                    .checked_div(100)
                    .map_or(0, |index| index),
            )
            .copied()
    };
    json!({"n":length,"p50_ms":percentile(50),"p95_ms":percentile(95),"p99_ms":percentile(99),"max_ms":samples.last(),"samples_ms":samples})
}

async fn benchmark(
    database: &Database,
    request: &CrawlRequest,
    arguments: &Arguments,
) -> Result<Value> {
    let now = request
        .deadline_ms
        .checked_sub(60000)
        .context("benchmark timestamp overflow")?;
    let start = Instant::now();
    let oracle = expected(database, request, now).await?;
    let oracle_ms = start.elapsed().as_secs_f64() * 1000.0;
    let (first, cold_ms) = measured(database, request, now).await?;
    ensure!(first == oracle, "cold selection differs from SQL oracle");
    let warm = stream::iter((0..arguments.samples).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut samples, _| async move {
            let (selected, elapsed) = measured(database, request, now).await?;
            ensure!(selected == oracle, "warm selection differs from SQL oracle");
            samples.push(elapsed);
            Ok(samples)
        })
        .await?;
    let mut result = json!({"oracle_ms":oracle_ms,"cold_prepare_and_selection_ms":cold_ms,"warm_selection":statistics(warm),"network_acquisition":false});
    if arguments.mutate_replica {
        let page = first.context("replica mutation benchmark needs an eligible candidate")?;
        let status: String = sqlx::query_scalar("SELECT status FROM scrape_pages WHERE id=$1")
            .bind(page)
            .fetch_one(database.pool())
            .await?;
        let changed = changed(database, request, arguments.samples, now, page, &status).await;
        let restore = sqlx::query("UPDATE scrape_pages SET status=$2 WHERE id=$1")
            .bind(page)
            .bind(&status)
            .execute(database.pool())
            .await;
        restore.context("restoring original replica checkpoint status")?;
        let object = result
            .as_object_mut()
            .context("benchmark report must be an object")?;
        object.insert("committed_change_selection".into(), statistics(changed?));
        object.insert(
            "canonical_changes".into(),
            changes::run(database, request, arguments.samples, now).await?,
        );
    }
    Ok(result)
}

async fn changed(
    database: &Database,
    request: &CrawlRequest,
    count: usize,
    now: i64,
    page: i64,
    status: &str,
) -> Result<Vec<f64>> {
    stream::iter((0..count).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut samples, index| async move {
            let next = if index.checked_rem(2) == Some(0) {
                "succeeded"
            } else {
                status
            };
            sqlx::query("UPDATE scrape_pages SET status=$2 WHERE id=$1")
                .bind(page)
                .bind(next)
                .execute(database.pool())
                .await?;
            let (selected, elapsed) = measured(database, request, now).await?;
            if index.checked_rem(10) == Some(0) {
                ensure!(
                    selected == expected(database, request, now).await?,
                    "committed-change selection differs from SQL oracle"
                );
            }
            samples.push(elapsed);
            Ok(samples)
        })
        .await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    if arguments.diagnostics {
        use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::io::stderr)
                    .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                        metadata
                            .target()
                            .starts_with("track_scrapper_v2::database::coverage_scheduler")
                    })),
            )
            .init();
    }
    ensure!(
        (1..=1000).contains(&arguments.samples),
        "samples must be between 1 and 1000"
    );
    let mut database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let mut parsed = url::Url::parse(&database_url)?;
    if arguments.production_read_only {
        ensure!(
            !arguments.mutate_replica
                && parsed.port() == Some(55432)
                && parsed.path() == "/scraper_v2_wi_public"
                && arguments.samples <= 120,
            "production qualification requires the known database, no mutations and at most120 samples"
        );
        let options = parsed
            .query_pairs()
            .find(|(key, _)| key == "options")
            .map_or_else(String::new, |(_, value)| value.into_owned());
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        parsed.set_query(None);
        parsed.query_pairs_mut().extend_pairs(pairs).append_pair(
            "options",
            &format!("{options} -cdefault_transaction_read_only=on"),
        );
        database_url = parsed.into();
    } else {
        ensure!(
            parsed.port() == Some(55434) && parsed.path() == "/trial_restore",
            "qualification requires the isolated trial_restore database on port55434"
        );
    }
    let mut request: CrawlRequest = serde_json::from_slice(&std::fs::read(&arguments.request)?)?;
    if let Some(state) = &arguments.state {
        ensure!(
            LOWER_48.contains(&state.as_str()),
            "qualification state must be in LOWER_48"
        );
        request.config.states = vec![state.clone()];
    }
    let database = if arguments.production_read_only {
        Database::connect_for_export(&database_url).await?
    } else {
        Database::connect(&database_url).await?
    };
    if arguments.production_read_only {
        let readonly: bool =
            sqlx::query_scalar("SELECT current_setting('default_transaction_read_only')='on'")
                .fetch_one(database.pool())
                .await?;
        ensure!(
            readonly,
            "production qualification must use read-only connections"
        );
    }
    let result = benchmark(&database, &request, &arguments).await;
    database.close().await;
    let report = result?;
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "{}",
        json!({"oracle_ms":report.get("oracle_ms"),"cold_ms":report.get("cold_prepare_and_selection_ms"),"warm_p50_ms":report.pointer("/warm_selection/p50_ms"),"warm_max_ms":report.pointer("/warm_selection/max_ms"),"change_p50_ms":report.pointer("/committed_change_selection/p50_ms"),"change_max_ms":report.pointer("/committed_change_selection/max_ms"),"canonical_changes":report.get("canonical_changes")})
    );
    ensure!(
        report
            .pointer("/warm_selection/max_ms")
            .and_then(Value::as_f64)
            .is_some_and(|maximum| maximum <= arguments.max_selection_ms),
        "warm selection exceeded the qualification limit"
    );
    if arguments.mutate_replica {
        ensure!(
            report
                .pointer("/committed_change_selection/max_ms")
                .and_then(Value::as_f64)
                .is_some_and(|maximum| maximum <= arguments.max_selection_ms),
            "committed-change selection exceeded the qualification limit"
        );
        changes::check(&report, arguments.max_selection_ms)?;
    }
    Ok(())
}
