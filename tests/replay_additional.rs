#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

fn bio_target() -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Athlete,
        "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0".to_owned(),
        PageContext {
            athlete_id: Some("28872883".to_owned()),
            state: Some("WI".to_owned()),
            sport: Some(Sport::Outdoor),
            ..PageContext::default()
        },
    )
}

fn bio_capture(target: &Target) -> Result<Capture> {
    Ok(Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".to_owned(),
        body: std::fs::read(support::fixture("athleticnet/bio-28872883.json"))?,
    })
}

#[tokio::test]
async fn interrupted_attempt_is_resumed_after_database_reconnection_without_losing_attempt_count()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let target = bio_target();
            let database = Database::connect(&test.url).await?;
            database
                .enqueue(std::slice::from_ref(&target), false)
                .await?;
            let job = database
                .next_job(Some(Source::AthleticNet), 3)
                .await?
                .context("initial job")?;
            assert!(database.start_attempt(job.id, 3).await?);
            database.close().await;

            let database = Database::connect(&test.url).await?;
            let resumed = database
                .next_job(Some(Source::AthleticNet), 3)
                .await?
                .context("interrupted job")?;
            assert_eq!(resumed.id, job.id);
            assert_eq!(resumed.target.url, target.url);
            assert!(database.start_attempt(resumed.id, 3).await?);
            let capture = bio_capture(&target)?;
            let page = sources::parse(&target, std::str::from_utf8(&capture.body)?)?;
            database
                .persist(Some(resumed.id), &target, &capture, &page)
                .await?;

            let state: (String, i32, bool) = sqlx::query_as(
                "SELECT status, attempts, last_success IS NOT NULL FROM scrape_pages WHERE id=$1",
            )
            .bind(job.id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(state, ("succeeded".to_owned(), 2, true));
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 53);
            let mark: String = sqlx::query_scalar(
                "SELECT raw_mark FROM results WHERE source_result_id='258445410'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(mark, "23.33a");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn checkpoint_mismatch_rolls_back_corrected_results_and_discovered_pages() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = bio_target();
            let capture = bio_capture(&target)?;
            let mut page = sources::parse(&target, std::str::from_utf8(&capture.body)?)?;
            let page_id = database.persist(None, &target, &capture, &page).await?;
            let other = records::target(Source::AthleticNet, "rollback-other");
            database
                .enqueue(std::slice::from_ref(&other), false)
                .await?;
            let other_id: i64 = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE url=$1")
                .bind(&other.url)
                .fetch_one(&test.pool)
                .await?;
            let before: Vec<(String, String)> = sqlx::query_as(
                "SELECT source_result_key,raw_mark FROM results ORDER BY source_result_key",
            )
            .fetch_all(&test.pool)
            .await?;
            let state_before: (String, Option<String>, Vec<String>) = sqlx::query_as(
                "SELECT status,last_success::text,notes FROM scrape_pages WHERE id=$1",
            )
            .bind(page_id)
            .fetch_one(&test.pool)
            .await?;
            let correction = page
                .performances
                .iter_mut()
                .find(|item| item.result_id.as_deref() == Some("258445410"))
                .context("fixture result to correct")?;
            correction.mark = "22.11".to_owned();
            let discovered = records::target(Source::AthleticNet, "rollback-new-discovery");
            page.next.push(discovered.clone());
            page.notes = vec!["uncommitted note".to_owned()];

            let error = database
                .persist(Some(other_id), &target, &capture, &page)
                .await
                .err()
                .context("mismatched checkpoint unexpectedly succeeded")?;
            assert!(format!("{error:#}").contains("checkpoint ID does not match"));
            let after: Vec<(String, String)> = sqlx::query_as(
                "SELECT source_result_key,raw_mark FROM results ORDER BY source_result_key",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(after, before);
            let state_after: (String, Option<String>, Vec<String>) = sqlx::query_as(
                "SELECT status,last_success::text,notes FROM scrape_pages WHERE id=$1",
            )
            .bind(page_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(state_after, state_before);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE url=$1")
                .bind(&discovered.url)
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 0);
            let other_state: (String, i32) =
                sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE id=$1")
                    .bind(other_id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(other_state, ("pending".to_owned(), 0));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn blocked_source_stops_cli_before_attempting_other_pending_pages() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let blocked = records::target(Source::AthleticNet, "blocked-first");
            let pending = records::target(Source::AthleticNet, "pending-second");
            database
                .enqueue(&[blocked.clone(), pending.clone()], false)
                .await?;
            let job = database
                .next_job(Some(Source::AthleticNet), 3)
                .await?
                .context("blocked job")?;
            assert_eq!(job.target.url, blocked.url);
            assert!(database.start_attempt(job.id, 3).await?);
            database.fail_job(&job, None, "access denied", true).await?;
            database.close().await;

            let output = test.cli(&["scrape"]).await?;
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("blocked page requiring human review")
            );
            let blocked_state: (String, i32, Option<String>) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE url=$1")
                    .bind(&blocked.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                blocked_state,
                ("blocked".to_owned(), 1, Some("access denied".to_owned()))
            );
            let pending_state: (String, i32, Option<String>) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE url=$1")
                    .bind(&pending.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(pending_state, ("pending".to_owned(), 0, None));
            Ok(())
        })
    })
    .await
}
