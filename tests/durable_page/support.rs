use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::page::{self, PageRequest, PageReservation, PageScope, ReservationDecision},
    model::{Capture, PageContext, PageKind, Source, Target},
};

pub fn config() -> Result<Config> {
    Config::parse(
        "states=['OH']\ngraduation_years=[2027]\nseason=2026\n[acquisition]\nmode='public_results'\nprofile_coverage_only=true\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\nenabled=true\ntransport='http'\n",
    )
}

pub fn scope(config: &Config) -> Result<PageScope> {
    Ok(PageScope {
        run_generation: "durable-page-test".into(),
        config_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(config)?)),
        deadline_ms: chrono::Utc::now()
            .timestamp_millis()
            .checked_add(3_600_000)
            .context("test deadline")?,
        cooldown_ms: 15_000,
    })
}

pub fn target(id: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Team,
        format!("https://oh.milesplit.com/teams/{id}"),
        PageContext {
            state: Some("OH".into()),
            school_id: Some(id.into()),
            ..Default::default()
        },
    )
}

pub async fn request(
    database: &Database,
    target: &Target,
    scope: &PageScope,
) -> Result<PageRequest> {
    database
        .enqueue(std::slice::from_ref(target), false)
        .await?;
    let id = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE cursor=$1")
        .bind(serde_json::to_value(target)?)
        .fetch_one(database.pool())
        .await?;
    page::request(
        track_scrapper_v2::database::Job {
            id,
            target: target.clone(),
        },
        scope,
    )
}

pub async fn reserve(
    database: &Database,
    request: &PageRequest,
    now: i64,
) -> Result<PageReservation> {
    match page::reserve(database, request, now).await? {
        ReservationDecision::Reserved { reservation } => Ok(*reservation),
        other => anyhow::bail!("expected reservation, got {other:?}"),
    }
}

pub fn profile() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://oh.milesplit.com/athletes/15005189-brice-fuller".into(),
        PageContext {
            state: Some("OH".into()),
            athlete_id: Some("15005189".into()),
            ..Default::default()
        },
    )
}

pub fn capture() -> Result<Capture> {
    Ok(Capture {
        url: profile().url,
        status: 200,
        content_type: "text/html".into(),
        body: std::fs::read(crate::support::fixture(
            "milesplit/athlete-oh-15005189.html",
        ))?,
    })
}

pub async fn snapshots(database: &Database) -> Result<serde_json::Value> {
    sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(p) ORDER BY operation_id) FROM durable_page_operations p),'attempts',(SELECT jsonb_agg(to_jsonb(p) ORDER BY attempt_id) FROM durable_page_attempts p),'receipts',(SELECT jsonb_agg(to_jsonb(p) ORDER BY operation_id) FROM durable_page_receipts p),'results',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM results p),'athletes',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athletes p),'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p))")
        .fetch_one(database.pool()).await.context("snapshot durable state")
}
