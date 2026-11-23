use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::Value;

use crate::{
    config::Config,
    database::Database,
    durable::model::{CrawlPhase, CrawlRequest, SchedulerVersion},
    model::{PageContext, PageKind, Source, Target},
    native_ops::recovery::runtime::Runtime,
};

pub(super) struct Seeded {
    pub request: CrawlRequest,
    pub pages: Vec<i64>,
    pub checkpoints: Value,
    pub exclusions: u32,
}

pub(super) async fn prepare(
    runtime: &Runtime,
    identity: &str,
    state: &str,
    scheduler: Option<SchedulerVersion>,
    concurrency: usize,
    exclusions: u32,
) -> Result<Seeded> {
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scrape_pages WHERE upper(COALESCE(cursor->'context'->>'acquisition_state',cursor->'context'->>'state'))=$1 OR url LIKE $2",
    )
    .bind(state)
    .bind(format!("https://{}.milesplit.com/%", state.to_lowercase()))
    .fetch_one(&runtime.pool)
    .await?;
    ensure!(
        existing == 0,
        "policy fixture state already has checkpoints"
    );
    let config = Config::parse(&format!(
        "states=['{state}']\ngraduation_years=[2027]\n[acquisition]\nmode='public_results'\ncollect_contacts=false\n[limits]\nrequests_per_second=3.0\nconcurrency={concurrency}\nmax_pages=3\n[sources.athleticnet]\nenabled=false\n[export]\nname='policy-progress-{}'\nlayout='single_sheet'",
        state.to_lowercase()
    ))?;
    let database = Database::connect_for_export(&runtime.database).await?;
    let result = pages(runtime, &database, &config, state, exclusions).await;
    database.close().await;
    let pages = result?;
    let checkpoints = super::super::handoff::checkpoints(runtime, &pages).await?;
    let request = CrawlRequest::new(
        format!("policy-{}-{identity}", state.to_lowercase()),
        config,
        chrono::Utc::now()
            .timestamp_millis()
            .checked_add(3_600_000)
            .context("policy proof deadline overflow")?,
        15_000,
        2,
        CrawlPhase::SchoolChains,
        Vec::new(),
    )?;
    let request = match scheduler {
        Some(SchedulerVersion::RollingV2) => request.upgraded_scheduler(),
        None => request,
    };
    request.validate(chrono::Utc::now().timestamp_millis())?;
    Ok(Seeded {
        request,
        pages,
        checkpoints,
        exclusions,
    })
}

async fn pages(
    runtime: &Runtime,
    database: &Database,
    config: &Config,
    state: &str,
    count: u32,
) -> Result<Vec<i64>> {
    let seeds = crate::sources::seeds(Source::MileSplit, &config.states, config.season)?;
    database.enqueue(&seeds, false).await?;
    let urls: Vec<_> = seeds.iter().map(|target| target.url.as_str()).collect();
    sqlx::query("UPDATE scrape_pages SET status='succeeded',last_success=clock_timestamp() WHERE source='milesplit' AND url=ANY($1)")
        .bind(urls).execute(&runtime.pool).await?;
    let first = insert(
        runtime,
        database,
        state,
        "meets/999001-policy-first",
        "failed",
    )
    .await?;
    let mut pages = stream::iter((1..=count).map(Ok::<_, anyhow::Error>))
        .try_fold(vec![first], |mut pages, id| async move {
            pages.push(
                insert(
                    runtime,
                    database,
                    state,
                    &format!("api/v1/meets/{id}/performances"),
                    "pending",
                )
                .await?,
            );
            Ok(pages)
        })
        .await?;
    pages.push(
        insert(
            runtime,
            database,
            state,
            "meets/999002-policy-last",
            "failed",
        )
        .await?,
    );
    Ok(pages)
}

async fn insert(
    runtime: &Runtime,
    database: &Database,
    state: &str,
    path: &str,
    status: &str,
) -> Result<i64> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Meet,
        format!("https://{}.milesplit.com/{path}", state.to_lowercase()),
        PageContext {
            state: Some(state.into()),
            ..PageContext::default()
        },
    );
    database
        .enqueue(std::slice::from_ref(&target), false)
        .await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM scrape_pages WHERE source='milesplit' AND url=$1 ORDER BY id LIMIT 2",
    )
    .bind(&target.url)
    .fetch_all(&runtime.pool)
    .await?;
    ensure!(ids.len() == 1, "policy fixture page identity ambiguous");
    let id = *ids.first().context("policy fixture page absent")?;
    sqlx::query("UPDATE scrape_pages SET status=$2,attempts=3,last_error='unsupported fixture format' WHERE id=$1")
        .bind(id).bind(status).execute(&runtime.pool).await?;
    Ok(id)
}
