#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    config::Config,
    database::{
        Database,
        durable::{SavedStages, StageEvidence},
    },
    durable::{
        blocking::BlockingStages,
        model::BatchProgress,
        page::{self, PageFiles, PageOutcome, PageReservation, PageScope},
    },
    model::{Capture, Sport, Target},
    sources::athleticnet,
};

fn config() -> Result<Config> {
    Config::parse(
        "states=['MI']\ngraduation_years=[2028]\nseason=2026\n[acquisition]\nmode='public_results'\n[sources.athleticnet]\nenabled=true\ntransport='http'\n[sources.milesplit]\nenabled=false\n",
    )
}

fn target(athlete: u64) -> Result<Target> {
    athleticnet::athlete_target(&athlete.to_string(), Some("MI"), Sport::Outdoor)
}

fn body(athlete: u64, first_result: u64, rows: usize) -> Value {
    let results: Vec<_> = (0..rows)
        .map(|index| {
            json!({
                "IDResult":first_result + u64::try_from(index).expect("small fixture"),
                "AthleteID":athlete,"SchoolID":3204,"EventID":1,
                "EventTypeID":0,"SeasonID":2026,"MeetID":1000,
                "ResultDate":"2026-05-01T00:00:00","Result":"11.20a"
            })
        })
        .collect();
    json!({
        "athlete":{"IDAthlete":athlete,"SchoolID":3204,
            "FirstName":format!("Runner {athlete}"),"LastName":"Synthetic","Gender":"M","State":"MI"},
        "allTeams":{"3204":{"IDSchool":3204,"SchoolName":"Synthetic High","State":"MI"}},
        "allSeasons":[{"SchoolID":3204,"IDSeason":2026,"Display":"2026 Outdoor"}],
        "grades":{"3204_2026":10},
        "meets":{"1000":{"IDMeet":1000,"MeetName":"Synthetic Meet","EndDate":"2026-05-01T00:00:00"}},
        "eventsTF":[{"IDEvent":1,"IDEventType":0,"Event":"100 Meters","PersonalEvent":true}],
        "resultsTF":results,"resultsXC":[],"relayTeamMembers":[]
    })
}

async fn replay(
    database: &Database,
    files: PageFiles<'_>,
    scope: &PageScope,
    athlete: u64,
    body: &Value,
) -> Result<PageOutcome> {
    let target = target(athlete)?;
    page::offline_replay(
        database,
        files,
        scope,
        target.clone(),
        Capture {
            url: target.url,
            status: 200,
            content_type: "application/json".into(),
            body: serde_json::to_vec(body)?,
        },
    )
    .await
}

async fn persisted(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('schools',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM schools s),'athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a),'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p),'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r))")
        .fetch_one(database.pool()).await?)
}

async fn rejection_replays(
    database: &Database,
    files: PageFiles<'_>,
    scope: &PageScope,
    athlete: u64,
    outcome: &PageOutcome,
) -> Result<()> {
    let request = page_support::request(database, &target(athlete)?, scope).await?;
    let row: (String, i32, i32, i32) = sqlx::query_as("SELECT attempt_id,cycle,ordinal,total_attempts FROM durable_page_attempts WHERE operation_id=$1")
        .bind(request.binding.operation_id()).fetch_one(database.pool()).await?;
    let reservation = PageReservation {
        request,
        attempt_id: row.0,
        cycle: row.1,
        ordinal: row.2,
        total_attempts: row.3,
    };
    let SavedStages::Settled(settled) = database.durable_stages(&reservation).await? else {
        anyhow::bail!("rejected page must settle its original attempt")
    };
    let (StageEvidence::Parsed { capture, parsed }, saved) = settled.into_parts() else {
        anyhow::bail!("source data rejection must retain exact capture and parse")
    };
    assert_eq!(&saved, outcome);
    let before = page_support::snapshots(database).await?;
    assert_eq!(
        page::persist(database, files, &reservation, &capture, &parsed).await?,
        saved
    );
    assert_eq!(page_support::snapshots(database).await?, before);
    let mut altered = reservation.clone();
    altered.request.target.context.state = Some("CO".into());
    assert!(
        page::persist(database, files, &altered, &capture, &parsed)
            .await
            .is_err()
    );
    assert_eq!(page_support::snapshots(database).await?, before);
    Ok(())
}

async fn conflicting_result_page(rows: usize) -> Result<()> {
    support::with_schema(move |test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let scope = page_support::scope(&config()?)?;
        let blocking = BlockingStages::new();
        let root = test.directory().join("artifacts");
        let files = PageFiles::new(&root, &blocking);
        let first = replay(&database, files, &scope, 381, &body(381, 42, rows)).await?;
        assert!(matches!(first, PageOutcome::Completed { .. }));
        let before = persisted(&database).await?;
        let rejected = replay(&database, files, &scope, 382, &body(382, 42, rows)).await?;
        let PageOutcome::Rejected { detail } = &rejected else { anyhow::bail!("expected rejection, got {rejected:?}") };
        assert!(detail.contains("different athlete"), "{detail}");
        assert_eq!(persisted(&database).await?, before);
        rejection_replays(&database, files, &scope, 382, &rejected).await?;
        let final_page = replay(&database, files, &scope, 383, &body(383, 1000, rows)).await?;
        assert!(matches!(final_page, PageOutcome::Completed { .. }));
        let progress = [first, rejected, final_page].iter().enumerate().try_fold(BatchProgress::default(), |progress, (index, outcome)| progress.record(i64::try_from(index)?, outcome))?;
        assert_eq!((progress.selected, progress.completed, progress.rejected), (3, 2, 1));
        let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM results),(SELECT count(*) FROM athletes),(SELECT count(*) FROM durable_page_receipts),(SELECT count(*) FROM durable_page_attempts)")
            .fetch_one(database.pool()).await?;
        assert_eq!(counts, (i64::try_from(rows * 2)?, 2, 2, 3));
        blocking.drain().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_cross_athlete_scalar_result_id_when_persisted_then_page_rejects_and_next_page_completes()
-> Result<()> {
    conflicting_result_page(1).await
}

#[tokio::test]
async fn given_cross_athlete_batch_result_ids_when_persisted_then_page_rejects_and_next_page_completes()
-> Result<()> {
    conflicting_result_page(8).await
}

async fn nul_page(raw_json: bool) -> Result<()> {
    support::with_schema(move |test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let scope = page_support::scope(&config()?)?;
        let blocking = BlockingStages::new();
        let root = test.directory().join("artifacts");
        let files = PageFiles::new(&root, &blocking);
        let mut malformed = body(382, 42, 1);
        if raw_json {
            malformed["resultsTF"][0]["PublishedNote"] = json!("unexpected\u{0}metadata");
        } else {
            malformed["allTeams"]["3204"]["SchoolName"] = json!("Synthetic\u{0}High");
        }
        let rejected = replay(&database, files, &scope, 382, &malformed).await?;
        let PageOutcome::Rejected { detail } = &rejected else { anyhow::bail!("expected rejection, got {rejected:?}") };
        assert!(detail.contains("PostgreSQL"), "{detail}");
        let empty: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM results),(SELECT count(*) FROM athletes),(SELECT count(*) FROM schools),(SELECT count(*) FROM durable_page_receipts)")
            .fetch_one(database.pool()).await?;
        assert_eq!(empty, (0, 0, 0, 0));
        rejection_replays(&database, files, &scope, 382, &rejected).await?;
        let completed = replay(&database, files, &scope, 383, &body(383, 1000, 1)).await?;
        assert!(matches!(completed, PageOutcome::Completed { .. }));
        let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_page_attempts WHERE state='failed' AND capture IS NOT NULL AND parsed IS NOT NULL")
            .fetch_one(database.pool()).await?;
        assert_eq!(retained, 1);
        blocking.drain().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_nul_in_raw_json_when_persisted_then_rejection_is_retained_and_next_page_completes()
-> Result<()> {
    nul_page(true).await
}

#[tokio::test]
async fn given_nul_in_published_school_when_persisted_then_rejection_is_retained_and_next_page_completes()
-> Result<()> {
    nul_page(false).await
}

#[tokio::test]
async fn given_nul_in_parser_diagnostic_when_recording_rejection_then_next_page_completes()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let scope = page_support::scope(&config()?)?;
        let blocking = BlockingStages::new();
        let root = test.directory().join("artifacts");
        let files = PageFiles::new(&root, &blocking);
        let mut malformed = body(382, 42, 1);
        malformed["allTeams"]["bad\u{0}id"] = json!({"SchoolName":"Invalid identity"});
        let rejected = replay(&database, files, &scope, 382, &malformed).await?;
        let PageOutcome::Rejected { detail } = &rejected else { anyhow::bail!("expected parser rejection, got {rejected:?}") };
        assert_eq!(detail, "invalid Athletic.net identity: bad\\0id");
        assert!(!detail.contains('\0'));
        let before = page_support::snapshots(&database).await?;
        assert_eq!(replay(&database, files, &scope, 382, &malformed).await?, rejected);
        assert_eq!(page_support::snapshots(&database).await?, before);
        let stages: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM durable_page_attempts WHERE state='failed' AND capture IS NOT NULL AND parsed IS NULL),(SELECT count(*) FROM results),(SELECT count(*) FROM durable_page_receipts)")
            .fetch_one(database.pool()).await?;
        assert_eq!(stages, (1, 0, 0));
        assert!(matches!(replay(&database, files, &scope, 383, &body(383, 1000, 1)).await?, PageOutcome::Completed { .. }));
        blocking.drain().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_oversized_parsed_expansion_when_encoding_then_page_rejects_and_next_page_completes()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let scope = page_support::scope(&config()?)?;
        let blocking = BlockingStages::new();
        let root = test.directory().join("artifacts");
        let files = PageFiles::new(&root, &blocking);
        let mut oversized = body(382, 42, 65);
        oversized["eventsTF"][0]["PublishedMetadata"] = json!("a".repeat(1024 * 1024));
        assert!(serde_json::to_vec(&oversized)?.len() < 2 * 1024 * 1024);
        let rejected = replay(&database, files, &scope, 382, &oversized).await?;
        let PageOutcome::Rejected { detail } = &rejected else { anyhow::bail!("expected size rejection, got {rejected:?}") };
        assert_eq!(detail, "parsed page exceeds the 64 MiB durable artifact grant");
        let before = page_support::snapshots(&database).await?;
        assert_eq!(replay(&database, files, &scope, 382, &oversized).await?, rejected);
        assert_eq!(page_support::snapshots(&database).await?, before);
        let stages: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM durable_page_attempts WHERE state='failed' AND capture IS NOT NULL AND parsed IS NULL),(SELECT count(*) FROM results),(SELECT count(*) FROM durable_page_receipts)")
            .fetch_one(database.pool()).await?;
        assert_eq!(stages, (1, 0, 0));
        assert!(matches!(replay(&database, files, &scope, 383, &body(383, 1000, 1)).await?, PageOutcome::Completed { .. }));
        blocking.drain().await?;
        database.close().await;
        Ok(())
    })).await
}

async fn unindexable_event_page(bytes: usize) -> Result<()> {
    support::with_schema(move |test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let scope = page_support::scope(&config()?)?;
        let blocking = BlockingStages::new();
        let root = test.directory().join("artifacts");
        let files = PageFiles::new(&root, &blocking);
        let label: String = (0_u64..u64::try_from(bytes / 64)?)
            .map(|index| format!("{:x}", Sha256::digest(index.to_le_bytes())))
            .collect();
        assert_eq!(label.len(), bytes);
        let mut malformed = body(382, 42, 1);
        malformed["eventsTF"][0]["Event"] = json!(label);
        let result = replay(&database, files, &scope, 382, &malformed).await;
        if let Err(error) = &result {
            for cause in error.chain() {
                if let Some(sqlx::Error::Database(database)) = cause.downcast_ref::<sqlx::Error>() {
                    eprintln!("unindexable published event PostgreSQL diagnostic: {database:?}");
                }
            }
        }
        let rejected = result?;
        let PageOutcome::Rejected { detail } = &rejected else { anyhow::bail!("expected index-width rejection, got {rejected:?}") };
        assert!(detail.contains("PostgreSQL"), "{detail}");
        let empty: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM results),(SELECT count(*) FROM athletes),(SELECT count(*) FROM schools),(SELECT count(*) FROM durable_page_receipts)")
            .fetch_one(database.pool()).await?;
        assert_eq!(empty, (0, 0, 0, 0));
        rejection_replays(&database, files, &scope, 382, &rejected).await?;
        assert!(matches!(replay(&database, files, &scope, 383, &body(383, 1000, 1)).await?, PageOutcome::Completed { .. }));
        let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_page_attempts WHERE state='failed' AND capture IS NOT NULL AND parsed IS NOT NULL")
            .fetch_one(database.pool()).await?;
        assert_eq!(retained, 1);
        blocking.drain().await?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_unindexable_published_event_when_persisted_then_rejection_is_retained_and_next_page_completes()
-> Result<()> {
    unindexable_event_page(8192).await
}

#[tokio::test]
async fn given_unindexable_btree_leaf_event_when_persisted_then_rejection_is_retained_and_next_page_completes()
-> Result<()> {
    unindexable_event_page(4096).await
}
