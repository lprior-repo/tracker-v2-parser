#[path = "concurrent_result_writes/support.rs"]
mod concurrency;
#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use track_scrapper_v2::{
    database::Database,
    model::{PageKind, Target},
};

#[tokio::test]
async fn given_a_locked_checkpoint_when_nonrefresh_batch_repeats_it_then_new_work_enqueues_without_waiting()
-> Result<()> {
    enqueue_scenario(false).await
}

#[tokio::test]
async fn given_a_locked_checkpoint_when_contact_batch_repeats_it_then_new_work_enqueues_without_waiting()
-> Result<()> {
    enqueue_scenario(true).await
}

#[tokio::test]
async fn given_concurrent_profile_pages_when_one_rediscovers_the_owned_navigation_then_both_receipts_commit()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let profile = concurrency::Prepared::milesplit_profile_navigation(&database, "").await?;
            let stats = concurrency::Prepared::milesplit_profile_navigation(&database, "/stats").await?;
            assert_ne!(profile.page_id(), stats.page_id());
            assert!(profile.has_navigation_to(&stats)?);
            let (completed_first, outcomes) = concurrency::interleave(
                &database, &test.pool, &profile, &stats, concurrency::NAVIGATION_GATE,
            ).await?;
            assert!(!completed_first, "the statistics writer must own its checkpoint while waiting on shared domain facts");
            let ownership: (String, String, i32, bool) = sqlx::query_as(
                "SELECT a.name,p.source_id,p.graduation_year,bool_and(r.athlete_id=p.athlete_id) \
                 FROM athletes a JOIN athlete_sources p ON p.athlete_id=a.id \
                 JOIN results r ON r.athlete_source_id=p.id GROUP BY a.name,p.source_id,p.graduation_year",
            ).fetch_one(&test.pool).await?;
            assert_eq!(ownership, ("Brice Fuller".into(), "15005189".into(), 2028, true));
            let completed: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM durable_page_receipts r \
                 JOIN durable_page_attempts a ON a.attempt_id=r.attempt_id \
                 JOIN durable_page_operations d ON d.operation_id=r.operation_id \
                 WHERE a.operation_id=r.operation_id AND d.page_id=r.page_id \
                 AND a.state='completed' AND d.state='completed' AND r.capture=a.capture \
                 AND r.parsed=a.parsed AND r.outcome=a.outcome AND r.outcome=d.outcome",
            ).fetch_one(&test.pool).await?;
            assert_eq!(completed, 2);
            concurrency::assert_replay(&database, [&profile, &stats], &outcomes).await?;
            database.close().await;
            Ok(())
        })
    }).await
}

async fn enqueue_scenario(include_contact: bool) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let existing = page_support::target("401");
            assert_eq!(database.enqueue(&[existing.clone()], false).await?, 1);
            sqlx::query("UPDATE scrape_pages SET status='failed',attempts=2,last_error='retained network failure',notes=ARRAY['retained evidence'] WHERE url=$1")
                .bind(&existing.url).execute(&test.pool).await?;
            let mut fresh = page_support::target("402");
            if include_contact {
                fresh.kind = PageKind::Contacts;
                fresh.url = "https://school.example.edu/staff".into();
                fresh.context.subject_id = Some("contacts-depth:0".into());
            }
            let targets = [existing, fresh];
            let before = checkpoint(&test.pool, &targets[0]).await?;
            let (blocked, inserted) = enqueue_while_locked(&database, &test.pool, &targets).await?;
            let after = checkpoint(&test.pool, &targets[0]).await?;
            let fresh: (String, i32) = sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE url=$1")
                .bind(&targets[1].url).fetch_one(&test.pool).await?;
            let sequence: i64 = sqlx::query_scalar("SELECT last_value FROM scrape_pages_id_seq")
                .fetch_one(&test.pool).await?;
            database.close().await;
            assert!(!blocked, "a repeated nonrefresh target waited for its checkpoint owner");
            assert_eq!(inserted, 1);
            assert_eq!(after, before);
            assert_eq!(fresh, ("pending".into(), 0));
            assert_eq!(sequence, 3);
            Ok(())
        })
    }).await
}

async fn checkpoint(pool: &PgPool, target: &Target) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE url=$1")
            .bind(&target.url)
            .fetch_one(pool)
            .await?,
    )
}

async fn enqueue_while_locked(
    database: &Database,
    pool: &PgPool,
    targets: &[Target; 2],
) -> Result<(bool, u64)> {
    let mut owner = pool.begin().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *owner)
        .await?;
    sqlx::query("SELECT id FROM scrape_pages WHERE url=$1 FOR UPDATE")
        .bind(&targets[0].url)
        .execute(&mut *owner)
        .await?;
    let mut enqueue = std::pin::pin!(database.enqueue(targets, false));
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut enqueue => Ok::<_, anyhow::Error>((false, Some(result))),
            result = blocked_by(pool, pid) => {
                result?;
                Ok((true, None))
            }
        }
    })
    .await;
    let released = owner.rollback().await;
    let (blocked, result) =
        observed.context("enqueue neither completed nor reached its checkpoint lock")??;
    released?;
    let inserted = match result {
        Some(result) => result?,
        None => tokio::time::timeout(Duration::from_secs(5), &mut enqueue)
            .await
            .context("enqueue did not drain after releasing the checkpoint")??,
    };
    Ok((blocked, inserted))
}

async fn blocked_by(pool: &PgPool, pid: i32) -> Result<()> {
    loop {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
        )
        .bind(pid)
        .fetch_one(pool)
        .await?;
        if blocked {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}
