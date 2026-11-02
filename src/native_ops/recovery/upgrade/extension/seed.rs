use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

use crate::native_ops::recovery::runtime::Runtime;
use crate::{
    config::Config,
    database::{Database, Job},
    durable::{
        model::{CrawlPhase, CrawlRequest, DeadlineExtensionAuthorization},
        page::{self, FailureClass, FailureRecord, PageOutcome, ReservationDecision, RetryPolicy},
    },
    model::{PageContext, PageKind, Source, Target},
};

pub(super) struct Seeded {
    pub page_id: i64,
    pub request: CrawlRequest,
    pub previous_deadline_ms: i64,
    pub old_operation: Value,
    pub old_attempt_id: String,
}

pub(super) async fn prepare(runtime: &Runtime, identity: &str) -> Result<Seeded> {
    let generation = format!("extension-{identity}");
    let config_path = crate::native_ops::recovery::cli::config(
        runtime,
        "WY",
        1,
        1,
        "deadline-extension-config.toml",
    )?;
    let mut config = Config::load(&config_path)?;
    // This proof resumes a deliberately deferred owner. Profile coverage
    // scheduling selects only fresh school-sweep candidates and would skip it.
    config.acquisition.set_profile_coverage_only(false)?;
    let target = target();
    let database = Database::connect(&runtime.database).await?;
    database
        .enqueue(std::slice::from_ref(&target), false)
        .await?;
    let (page_id, cursor) = load_page(runtime, &target).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let previous_deadline_ms = now.checked_add(1_500).context("old deadline overflow")?;
    let deadline_ms = now
        .checked_add(3_600_000)
        .context("extended deadline overflow")?;
    let mut request = request(generation, config, deadline_ms);
    let (old_operation, old_attempt_id) = defer_old_operation(
        runtime,
        &database,
        page_id,
        cursor,
        &request,
        now,
        previous_deadline_ms,
    )
    .await?;
    wait_until_after(previous_deadline_ms).await?;
    request.deadline_extension = Some(DeadlineExtensionAuthorization {
        previous_deadline_ms,
        effective_deadline_ms: request.deadline_ms,
    });
    request.validate(chrono::Utc::now().timestamp_millis())?;
    Ok(Seeded {
        page_id,
        request,
        previous_deadline_ms,
        old_operation,
        old_attempt_id,
    })
}

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Team,
        "https://wy.milesplit.com/teams/942991-deadline-extension".into(),
        PageContext {
            state: Some("WY".into()),
            school_id: Some("942991".into()),
            season: Some(2026),
            ..Default::default()
        },
    )
}

const fn request(generation: String, config: Config, deadline_ms: i64) -> CrawlRequest {
    CrawlRequest {
        generation,
        config,
        deadline_ms,
        cooldown_ms: 15_000,
        batch: 0,
        max_batches: 1,
        phase: CrawlPhase::Complete,
        inherited_retry_campaigns: Vec::new(),
        scheduler_version: None,
        deadline_extension: None,
    }
}

async fn load_page(runtime: &Runtime, target: &Target) -> Result<(i64, Value)> {
    let (page_id, cursor): (i64, Value) =
        sqlx::query_as("SELECT id,cursor FROM scrape_pages WHERE cursor=$1")
            .bind(serde_json::to_value(target)?)
            .fetch_one(&runtime.pool)
            .await?;
    ensure!(page_id > 0, "deadline extension fixture page ID is invalid");
    Ok((page_id, cursor))
}

async fn defer_old_operation(
    runtime: &Runtime,
    database: &Database,
    page_id: i64,
    cursor: Value,
    request: &CrawlRequest,
    now_ms: i64,
    previous_deadline_ms: i64,
) -> Result<(Value, String)> {
    let mut old_scope = request.page_scope()?;
    old_scope.deadline_ms = previous_deadline_ms;
    let original_request = page::request(
        Job {
            id: page_id,
            target: serde_json::from_value(cursor)?,
        },
        &old_scope,
    )?;
    let initial =
        page::reserve_configured(database, &request.config, &original_request, now_ms).await?;
    let ReservationDecision::Reserved { reservation } = initial else {
        anyhow::bail!("initial deadline extension fixture reservation absent")
    };
    let policy = RetryPolicy::configured(&request.config, &original_request)?;
    let PageOutcome::Deferred { retry_at_ms, .. } = database
        .durable_fail_with_policy(
            &reservation,
            FailureRecord {
                class: FailureClass::Transient,
                detail: "controlled pre-extension transient failure",
                capture: None,
                stopped_origin: None,
                observed_at_ms: now_ms,
            },
            &policy,
        )
        .await?
    else {
        anyhow::bail!("initial deadline extension attempt was not deferred")
    };
    ensure!(
        retry_at_ms < previous_deadline_ms,
        "fixture retry is not due before old deadline"
    );
    let old_operation: Value = sqlx::query_scalar(
        "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
    )
    .bind(&original_request.binding.operation_id)
    .fetch_one(&runtime.pool)
    .await?;
    Ok((old_operation, reservation.attempt_id))
}

async fn wait_until_after(previous_deadline_ms: i64) -> Result<()> {
    let wait_ms = previous_deadline_ms
        .saturating_add(100)
        .saturating_sub(chrono::Utc::now().timestamp_millis());
    if wait_ms > 0 {
        tokio::time::sleep(Duration::from_millis(u64::try_from(wait_ms)?)).await;
    }
    Ok(())
}
