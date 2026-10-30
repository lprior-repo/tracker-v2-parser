use super::{Scheduling, select_scheduled};
use crate::{
    config::Config,
    database::Database,
    durable::{model::page_config_digest, pacing::test_support::with_database, page::PageScope},
    model::{PageContext, PageKind, Source, Target},
};
use anyhow::{Context, Result};
use serde_json::Value;

async fn seeded(
    database: &Database,
    count: u32,
    allowed: bool,
) -> Result<(Config, PageScope, Target)> {
    let config = Config::parse(
        "states=['WI']\n[acquisition]\nmode='public_results'\n[sources.athleticnet]\nenabled=false",
    )?;
    let target = |url| {
        Target::get(
            Source::MileSplit,
            PageKind::Meet,
            url,
            PageContext {
                state: Some("WI".into()),
                ..Default::default()
            },
        )
    };
    let mut targets = (1..=count)
        .map(|id| {
            target(format!(
                "https://wi.milesplit.com/api/v1/meets/{id}/performances"
            ))
        })
        .collect::<Vec<_>>();
    let public = target("https://wi.milesplit.com/meets/900-public-results".into());
    if allowed {
        targets.push(public.clone());
    }
    database.enqueue(&targets, false).await?;
    let scope = PageScope {
        run_generation: "bounded-policy-exclusions".into(),
        config_sha256: page_config_digest(&config)?,
        deadline_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
        cooldown_ms: 60_000,
    };
    Ok((config, scope, public))
}

async fn checkpoints(database: &Database) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p")
            .fetch_one(database.pool())
            .await?,
    )
}

async fn assert_excluded(database: &Database, count: i64) -> Result<()> {
    let ledger: (i64,i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM durable_page_operations WHERE state='rejected'),(SELECT count(*) FROM durable_page_attempts),(SELECT count(*) FROM durable_wire_requests),(SELECT count(*) FROM durable_page_receipts),(SELECT COALESCE(sum(attempts),0)::bigint FROM scrape_pages)").fetch_one(database.pool()).await?;
    assert_eq!(ledger, (count, 0, 0, 0, 0));
    Ok(())
}

#[tokio::test]
async fn bounded_policy_scan_retries_after_one_hundred_exclusions_and_reaches_the_next_allowed_page()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let (config, scope, public) = seeded(&database, 101, true).await?;
            let now = chrono::Utc::now().timestamp_millis();
            let before = checkpoints(&database).await?;
            let error = select_scheduled(
                &database,
                &config,
                &scope,
                now,
                &[],
                Scheduling::PublicBulkTurn,
            )
            .await
            .expect_err("bounded exclusion progress must request retry, never claim completion");
            assert!(error.is::<crate::durable::AcquisitionBusy>());
            assert!(crate::durable::retryable_error(&error));
            assert!(format!("{error:#}").contains("bounded policy exclusion scan"));
            assert_excluded(&database, 100).await?;
            assert_eq!(checkpoints(&database).await?, before);
            let selected = select_scheduled(
                &database,
                &config,
                &scope,
                now,
                &[],
                Scheduling::PublicBulkTurn,
            )
            .await?
            .context("allowed page after exclusion scan retry")?;
            assert_eq!(selected.target.url, public.url);
            assert_eq!(selected.binding.scope, scope);
            assert_excluded(&database, 101).await?;
            assert_eq!(checkpoints(&database).await?, before);
            let duplicate = select_scheduled(
                &database,
                &config,
                &scope,
                now,
                &[],
                Scheduling::PublicBulkTurn,
            )
            .await?
            .context("same unreserved allowed page")?;
            assert_eq!(duplicate.binding, selected.binding);
            assert_excluded(&database, 101).await?;
            assert_eq!(checkpoints(&database).await?, before);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn exact_exclusion_bound_requires_a_retry_before_genuine_queue_completion() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let (config, scope, _) = seeded(&database, 100, false).await?;
            let now = chrono::Utc::now().timestamp_millis();
            let before = checkpoints(&database).await?;
            let error = select_scheduled(&database, &config, &scope, now, &[], Scheduling::Resume)
                .await
                .expect_err("bound alone must not claim no eligible pages");
            assert!(error.is::<crate::durable::AcquisitionBusy>());
            assert!(crate::durable::retryable_error(&error));
            assert_excluded(&database, 100).await?;
            assert_eq!(checkpoints(&database).await?, before);
            assert!(
                select_scheduled(&database, &config, &scope, now, &[], Scheduling::Resume)
                    .await?
                    .is_none()
            );
            assert_excluded(&database, 100).await?;
            assert_eq!(checkpoints(&database).await?, before);
            Ok(())
        })
    })
    .await
}
