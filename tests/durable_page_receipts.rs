#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{
        Database,
        durable::{SavedStages, StageEvidence},
    },
    durable::page::{self, PageOutcome, ReservationDecision},
    model::{Capture, PageContext, PageKind, Source, Target},
};

#[tokio::test]
async fn committed_domain_effect_replays_after_ack_loss_without_duplicate_rows_or_attempts()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = page_support::config()?;
            let scope = page_support::scope(&config)?;
            let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
            let root = test.directory().join("durable-artifacts");
            let outcome = page::offline_replay(
                &database,
                track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                &scope,
                page_support::profile(),
                page_support::capture()?,
            )
            .await?;
            assert!(matches!(outcome, PageOutcome::Completed { .. }));
            let result_count: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            assert!(
                result_count >= 9,
                "public PR table must be retained, got {result_count}"
            );
            let before = page_support::snapshots(&database).await?;
            let request =
                page_support::request(&database, &page_support::profile(), &scope).await?;
            let reservation_row: (String, i32, i32, i32) = sqlx::query_as(
                "SELECT attempt_id,cycle,ordinal,total_attempts FROM durable_page_attempts",
            )
            .fetch_one(&test.pool)
            .await?;
            let reservation = page::PageReservation {
                request: request.clone(),
                attempt_id: reservation_row.0,
                cycle: reservation_row.1,
                ordinal: reservation_row.2,
                total_attempts: reservation_row.3,
            };
            let SavedStages::Settled(settled) = database.durable_stages(&reservation).await? else {
                anyhow::bail!("completed attempt must be settled");
            };
            let (StageEvidence::Parsed { capture, parsed }, saved) = settled.into_parts() else {
                anyhow::bail!("completed attempt must retain capture and parse");
            };
            assert_eq!(saved, outcome);
            assert_eq!(
                page::persist(
                    &database,
                    track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                    &reservation,
                    &capture,
                    &parsed
                )
                .await?,
                outcome
            );
            assert_eq!(page_support::snapshots(&database).await?, before);
            database.close().await;
            let restarted = Database::connect(&test.url).await?;
            assert_eq!(
                page::offline_replay(
                    &restarted,
                    track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                    &scope,
                    page_support::profile(),
                    page_support::capture()?
                )
                .await?,
                outcome
            );
            assert!(matches!(
                page::reserve(&restarted, &request, chrono::Utc::now().timestamp_millis()).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Completed { .. }
                }
            ));
            assert_eq!(page_support::snapshots(&restarted).await?, before);
            let records = restarted.athlete_records().await?;
            assert_eq!(records.len(), 1);
            assert_eq!(
                records[0].performances.len(),
                usize::try_from(result_count)?
            );
            assert_eq!(records[0].profiles, vec![page_support::profile().url]);
            let source = records[0].profile_details.first().context("source facts")?;
            assert_eq!(
                source
                    .source_values
                    .get("_capture")
                    .and_then(|value| value.get("sha256"))
                    .and_then(serde_json::Value::as_str),
                Some(capture.body.sha256.as_str())
            );
            restarted.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn corrupt_retained_artifact_is_refused_without_rewriting_committed_observations()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
            let root = test.directory().join("artifacts");
            page::offline_replay(
                &database,
                track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                &scope,
                page_support::profile(),
                page_support::capture()?,
            )
            .await?;
            let (reservation, capture, parsed) = saved(&database, &scope).await?;
            let before = page_support::snapshots(&database).await?;
            std::fs::write(
                root.join(format!("{}.blob", capture.body.sha256)),
                b"corrupted",
            )?;
            let error = page::persist(
                &database,
                track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                &reservation,
                &capture,
                &parsed,
            )
            .await
            .expect_err("corrupt capture must fail closed");
            assert!(format!("{error:#}").contains("size mismatch"));
            assert_eq!(page_support::snapshots(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn rejected_persistence_rolls_back_domain_changes_and_receipt_together() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
            let root = test.directory().join("artifacts");
            page::offline_replay(
                &database,
                track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                &scope,
                page_support::profile(),
                page_support::capture()?,
            )
            .await?;
            let (mut reservation, capture, parsed) = saved(&database, &scope).await?;
            reservation.request.binding.target_sha256 = "f".repeat(64);
            let before = page_support::snapshots(&database).await?;
            assert!(
                page::persist(
                    &database,
                    track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking),
                    &reservation,
                    &capture,
                    &parsed
                )
                .await
                .is_err()
            );
            assert_eq!(page_support::snapshots(&database).await?, before);
            let mut changed = page_support::profile();
            changed.context.state = Some("WI".into());
            let reused = page::request(
                track_scrapper_v2::database::Job {
                    id: reservation.request.binding.page_id,
                    target: changed,
                },
                &scope,
            )?;
            assert!(
                page::reserve(&database, &reused, chrono::Utc::now().timestamp_millis())
                    .await
                    .is_err()
            );
            assert_eq!(page_support::snapshots(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn parse_rejects_attempt_with_parsed_evidence_but_no_recorded_capture() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            use track_scrapper_v2::durable::{blocking::BlockingStages, page::PageFiles};

            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let blocking = BlockingStages::new();
            let root = test.directory().join("artifacts");
            let files = PageFiles::new(&root, &blocking);
            page::offline_replay(
                &database,
                files,
                &scope,
                page_support::profile(),
                page_support::capture()?,
            )
            .await?;
            let (reservation, capture, _) = saved(&database, &scope).await?;
            sqlx::query("UPDATE durable_page_attempts SET capture=NULL WHERE attempt_id=$1")
                .bind(&reservation.attempt_id)
                .execute(&test.pool)
                .await?;
            let before = page_support::snapshots(&database).await?;
            let error = page::parse(
                &database,
                files,
                &reservation,
                &capture,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .expect_err("saved parse without a capture is corrupt");
            assert!(format!("{error:#}").contains("recorded parsed stage has no capture"));
            assert_eq!(page_support::snapshots(&database).await?, before);
            blocking.drain().await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn saved(
    database: &Database,
    scope: &page::PageScope,
) -> Result<(page::PageReservation, page::CaptureRef, page::ParsedRef)> {
    let request = page_support::request(database, &page_support::profile(), scope).await?;
    let (attempt_id, cycle, ordinal, total_attempts): (String, i32, i32, i32) =
        sqlx::query_as("SELECT attempt_id,cycle,ordinal,total_attempts FROM durable_page_attempts")
            .fetch_one(database.pool())
            .await?;
    let reservation = page::PageReservation {
        request,
        attempt_id,
        cycle,
        ordinal,
        total_attempts,
    };
    let SavedStages::Settled(settled) = database.durable_stages(&reservation).await? else {
        anyhow::bail!("attempt must be settled");
    };
    let (StageEvidence::Parsed { capture, parsed }, _) = settled.into_parts() else {
        anyhow::bail!("settled attempt must retain capture and parse");
    };
    Ok((reservation, capture, parsed))
}

#[tokio::test]
async fn offline_replay_cannot_clear_a_preserved_access_stop() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
        let database=Database::connect(&test.url).await?;
        let scope=page_support::scope(&page_support::config()?)?;
        let target=Target::get(Source::MileSplit,PageKind::Athlete,"https://oh.milesplit.com/athletes/99999".into(),PageContext {state:Some("OH".into()),athlete_id:Some("99999".into()),..Default::default()});
        database.enqueue(std::slice::from_ref(&target),false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=2,last_error='403 retained stop'").execute(&test.pool).await?;
        let before=page_support::snapshots(&database).await?;
        assert!(page::offline_replay(&database,track_scrapper_v2::durable::page::PageFiles::new(&test.directory().join("artifacts"), &blocking),&scope,target.clone(),Capture {url:target.url,status:200,content_type:"text/html".into(),body:b"<html>replacement</html>".to_vec()}).await.is_err());
        assert_eq!(page_support::snapshots(&database).await?,before);
        database.close().await;
        Ok(())
    })).await
}
