mod fixture;

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, stream};
use serde_json::{Value, json};
use sqlx::ConnectOptions;

use super::*;
use crate::{
    config::Config,
    durable::{blocking::BlockingStages, model::page_config_digest, pacing::PersistentPacing},
};
use fixture::{Case, Fixture};

struct Scenario<'a> {
    database: &'a Database,
    config: &'a Config,
    files: page::PageFiles<'a>,
    root: &'a Path,
    fixture: &'a Fixture,
    pacing: &'a PersistentPacing,
    slow: bool,
}

impl Scenario<'_> {
    async fn attempt(
        &self,
        index: usize,
        request: &page::PageRequest,
        due: Option<i64>,
    ) -> Result<(usize, PageOutcome)> {
        if let Some(due) = due {
            let wait = due.saturating_sub(chrono::Utc::now().timestamp_millis());
            if wait > 0 {
                tokio::time::sleep(Duration::from_millis(u64::try_from(wait)?)).await;
            }
        }
        let now = chrono::Utc::now().timestamp_millis();
        let reservation =
            match page::reserve_configured(self.database, self.config, request, now).await? {
                ReservationDecision::Reserved { reservation } => reservation,
                ReservationDecision::Settled { outcome } => return Ok((index, outcome)),
            };
        let mut connection = Transport::new(Options {
            mode: Mode::Http,
            driver_url: None,
            profile: self.root.join("unused"),
            headless: true,
            rps: 3.0,
        })?
        .with_shared_persistent_pacing(self.pacing.clone())
        .with_http_fixture("https://oh.milesplit.com", self.fixture.address(index)?)?
        .with_http_test_budget(Duration::from_secs(8));
        let admission = crate::durable::admission::acquire(self.database, 3, false).await?;
        crate::runner::durable::check_stops(self.database, self.config).await?;
        let fetched = page::fetch_configured(
            self.database,
            self.files,
            &reservation,
            &mut connection,
            now,
            self.config,
        )
        .await;
        let closed = connection.close().await;
        let released = admission.rollback().await;
        closed?;
        released?;
        let capture = match fetched? {
            FetchDecision::Captured { capture } => capture,
            FetchDecision::Settled { outcome } => {
                let detail = self.database.durable_failure_detail(request).await?;
                eprintln!(
                    "settled_acquisition={}",
                    json!({"slow":self.slow,"index":index,"attempt":reservation.ordinal,"outcome":outcome,"detail":detail})
                );
                return Ok((index, outcome));
            }
        };
        let parsed = match page::parse(
            self.database,
            self.files,
            &reservation,
            &capture,
            chrono::Utc::now().timestamp_millis(),
        )
        .await?
        {
            ParseDecision::Parsed { parsed } => parsed,
            ParseDecision::Settled { outcome } => return Ok((index, outcome)),
        };
        Ok((
            index,
            page::persist(self.database, self.files, &reservation, &capture, &parsed).await?,
        ))
    }

    async fn wave(
        &self,
        requests: &[page::PageRequest],
        pending: Vec<(usize, Option<i64>)>,
    ) -> Result<Vec<(usize, PageOutcome)>> {
        let results = stream::iter(pending)
            .map(|(index, due)| async move {
                self.attempt(
                    index,
                    requests.get(index).context("fixture page absent")?,
                    due,
                )
                .await
            })
            .buffer_unordered(3)
            .collect::<Vec<_>>()
            .await;
        results.into_iter().collect()
    }
}

async fn scenario(pool: &sqlx::PgPool, slow: bool) -> Result<Value> {
    let count = if slow { 20 } else { 32 };
    let config = Config::parse(
        "states=['OH']\ngraduation_years=[2028]\n[limits]\nrequests_per_second=3.0\nconcurrency=3\nmax_attempts=3\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\ntransport='http'\n[acquisition]\nmode='public_results'",
    )?;
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await?;
    let mut address = pool.connect_options().to_url_lossy();
    address
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let database = Database::connect(address.as_str()).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT current_schema()")
            .fetch_one(database.pool())
            .await?,
        schema
    );
    let root = tempfile::tempdir()?;
    let blocking = BlockingStages::new();
    let scope = PageScope {
        config_sha256: page_config_digest(&config)?,
        run_generation: if slow {
            "slow-resilience"
        } else {
            "responsive-resilience"
        }
        .into(),
        ..scope()?
    };
    let mut requests = Vec::new();
    for index in 0..count {
        requests.push(page::bind_target(&database, fixture::target(index), &scope).await?);
    }
    let fixture = Fixture::start(count, slow).await?;
    let pacing = PersistentPacing::new(database.pool().clone()).with_overlapped_starts();
    let context = Scenario {
        database: &database,
        config: &config,
        files: page::PageFiles::new(root.path(), &blocking),
        root: root.path(),
        fixture: &fixture,
        pacing: &pacing,
        slow,
    };
    let work = async {
        let mut outcomes = context.wave(&requests, (0..count).map(|index| (index, None)).collect()).await?;
        let first_wave = wire_measurement(pool).await?;
        for _ in 0..2 {
            let pending = outcomes.iter().filter_map(|(index, outcome)| match outcome {
                PageOutcome::Deferred { retry_at_ms, .. } => Some((*index, Some(*retry_at_ms))),
                _ => None,
            }).collect::<Vec<_>>();
            let retried = context.wave(&requests, pending).await?;
            for (index, outcome) in retried {
                outcomes.iter_mut().find(|(candidate, _)| *candidate == index)
                    .context("retry lost its page")?.1 = outcome;
            }
        }
        let completed = outcomes.iter().filter(|(_, outcome)| matches!(outcome, PageOutcome::Completed {..})).count();
        let rejected = outcomes.iter().filter(|(_, outcome)| matches!(outcome, PageOutcome::Rejected {..})).count();
        let exhausted = outcomes.iter().filter(|(_, outcome)| matches!(outcome, PageOutcome::Exhausted {..})).count();
        let expected_rejected = (0..count).filter(|index| Case::for_index(*index, slow) == Case::Malformed).count();
        let expected_exhausted = (0..count).filter(|index| Case::for_index(*index, slow) == Case::Timeout).count();
        assert_eq!(rejected, expected_rejected, "{outcomes:?}");
        assert_eq!(exhausted, expected_exhausted, "{outcomes:?}");
        assert_eq!(completed, count - expected_rejected - expected_exhausted);
        assert_eq!(outcomes.len(), count);
        for (index, outcome) in &outcomes {
            let replayed = page::reserve_configured(&database, &config, &requests[*index], chrono::Utc::now().timestamp_millis()).await?;
            assert!(matches!(replayed, ReservationDecision::Settled { outcome: saved } if saved == *outcome));
        }
        let states: Vec<(String, i64)> = sqlx::query_as("SELECT state,count(*) FROM durable_page_operations GROUP BY state ORDER BY state").fetch_all(pool).await?;
        let attempts: Vec<i32> = sqlx::query_scalar("SELECT attempts FROM scrape_pages ORDER BY id").fetch_all(pool).await?;
        assert_eq!(attempts.len(), count);
        let expected_attempts: Vec<i32> = (0..count).map(|index| match Case::for_index(index, slow) {
            Case::Healthy | Case::Malformed => 1,
            Case::Unavailable | Case::Truncated => 2,
            Case::Timeout => 3,
        }).collect();
        assert_eq!(attempts, expected_attempts, "{outcomes:?}");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(pool).await?,i64::try_from(completed)?);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_operations WHERE state='active'").fetch_one(pool).await?,0);
        let wire = wire_measurement(pool).await?;
        assert_eq!(wire["wire_requests"], json!(if slow { 36 } else { 48 }));
        Ok::<_, anyhow::Error>(json!({
            "scenario":if slow {"slow_mixed"} else {"responsive_mixed"},
            "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
            "pages":count,"concurrency":3,"configured_rps":3.0,"max_attempts":3,
            "http_timeout_ms":8000,
            "completed":completed,"rejected":rejected,"exhausted":exhausted,
            "attempt_counts":attempts,"operation_states":states,
            "first_wave":first_wave,"including_retries":wire,
            "database_pool_max_connections":8,
            "scope":"real local HTTP, PostgreSQL acquisition admission, durable pacing, retained artifacts, parse, SQL persistence and page replay; bounded async page driver, no Restate root scheduling, no provider traffic"
        }))
    }.await;
    let finished = fixture.finish().await;
    let drained = blocking.drain().await;
    database.close().await;
    let mut report = work?;
    let observations = finished?;
    drained?;
    report["fixture_observations"] = json!(observations);
    Ok(report)
}

async fn wire_measurement(pool: &sqlx::PgPool) -> Result<Value> {
    let times: Vec<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT execute_started_at FROM durable_wire_requests WHERE execute_started_at IS NOT NULL ORDER BY execute_started_at").fetch_all(pool).await?;
    ensure!(
        times.len() >= 2,
        "measurement needs at least two actual starts"
    );
    let gaps = times
        .windows(2)
        .map(|pair| {
            (pair[1] - pair[0])
                .num_microseconds()
                .context("wire gap overflow")
        })
        .collect::<Result<Vec<_>>>()?;
    let span = (*times.last().context("last wire start")?
        - *times.first().context("first wire start")?)
    .num_microseconds()
    .context("wire span overflow")?;
    let mut sorted = gaps.clone();
    sorted.sort_unstable();
    ensure!(
        sorted[0] >= 333_333,
        "actual wire starts exceeded configured 3RPS: {sorted:?}"
    );
    let rate = (times.len() - 1) as f64 * 1_000_000.0 / span as f64;
    Ok(
        json!({"wire_requests":times.len(),"span_us":span,"requests_per_second":rate,
        "gap_min_us":sorted[0],"gap_p50_us":sorted[sorted.len()/2],
        "gap_p95_us":sorted[(sorted.len()-1)*95/100],"gap_max_us":sorted.last(),"gaps_us":gaps}),
    )
}

#[tokio::test]
#[ignore = "opt-in release mixed-failure throughput qualification against real PostgreSQL"]
async fn release_mixed_failure_page_cadence() -> Result<()> {
    let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    for slow in [false, true] {
        let collected = reports.clone();
        with_database(|pool| {
            Box::pin(async move {
                let report = scenario(pool, slow).await?;
                eprintln!("scenario_report={}", serde_json::to_string(&report)?);
                collected.lock().expect("measurement reports").push(report);
                Ok(())
            })
        })
        .await?;
    }
    let report = json!({"reports":*reports.lock().expect("measurement reports")});
    eprintln!("{}", serde_json::to_string_pretty(&report)?);
    if let Ok(path) = std::env::var("RESILIENCE_CADENCE_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    let first_rps = report
        .pointer("/reports/0/first_wave/requests_per_second")
        .and_then(Value::as_f64)
        .context("responsive first wave rate")?;
    ensure!(
        first_rps >= 2.7,
        "responsive mixed-page workload missed 90% of 3RPS: {first_rps}"
    );
    Ok(())
}
