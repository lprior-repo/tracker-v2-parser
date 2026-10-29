#[path = "durable_page/support.rs"]
mod page_support;
#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use records::{athlete, performance, persist, target};
use sqlx::Row;
use std::path::Path;
use support::{assert_success, fixture, import_bio, with_schema};
use track_scrapper_v2::database::Database;
use track_scrapper_v2::model::{Athlete, ParsedPage, Source};

fn sheet_rows(path: &Path, sheet: &str) -> Result<Vec<Vec<String>>> {
    let mut workbook = open_workbook_auto(path)?;
    let range = workbook.worksheet_range(sheet)?;
    Ok(range
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect())
}

fn cell<'a>(rows: &'a [Vec<String>], header: &str) -> Result<&'a str> {
    let headings = rows.first().context("missing workbook headers")?;
    let index = headings
        .iter()
        .position(|heading| heading == header)
        .with_context(|| format!("missing {header} column"))?;
    rows.get(1)
        .and_then(|row| row.get(index))
        .map(String::as_str)
        .context("missing workbook data cell")
}

async fn complete_seed_pages(database: &Database) -> Result<()> {
    let states = ["CA", "OH", "WI"].map(str::to_owned);
    for source in [Source::AthleticNet, Source::MileSplit] {
        let seeds = track_scrapper_v2::sources::seeds(source, &states, 2026)?;
        database.enqueue(&seeds, false).await?;
    }
    sqlx::query("UPDATE scrape_pages SET status='succeeded'")
        .execute(database.pool())
        .await?;
    Ok(())
}

fn failure_target(id: &str) -> Result<track_scrapper_v2::model::Target> {
    track_scrapper_v2::sources::athleticnet::athlete_target(
        id,
        Some("WI"),
        track_scrapper_v2::model::Sport::Outdoor,
    )
}

async fn missing_capture_failure(
    test: &support::Scenario,
    target: &track_scrapper_v2::model::Target,
) -> Result<()> {
    let input = test.directory().join("missing-acquisition-capture.json");
    let error = track_scrapper_v2::runner::Runner::new()
        .replay(
            &track_scrapper_v2::config::Config::default(),
            &test.url,
            &input,
            target.clone(),
            &test.directory().join("evidence"),
        )
        .await
        .err()
        .context("missing replay capture must fail acquisition")?;
    assert!(format!("{error:#}").contains("opening replay input"));
    Ok(())
}

#[tokio::test]
async fn given_real_career_when_replayed_twice_then_identity_marks_and_checkpoint_are_unique()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        assert_success(&import_bio(test).await?);
        assert_success(&import_bio(test).await?);
        let profiles: Vec<(String, String)> = sqlx::query_as("SELECT source_id,name FROM athlete_sources").fetch_all(&test.pool).await?;
        assert_eq!(profiles, vec![("28872883".to_owned(), "Kingston Penn".to_owned())]);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(count, 53);
        let assignment: (i64,i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE athlete_id IS NOT NULL),count(*) FILTER (WHERE athlete_id IS NULL) FROM results")
            .fetch_one(&test.pool).await?;
        assert_eq!(assignment,(41,12),"relay team marks must not become individual athlete results");
        let result = sqlx::query("SELECT event,raw_mark,meet_date,source_url FROM results WHERE source_result_id='258445410'").fetch_one(&test.pool).await?;
        assert_eq!(result.get::<String,_>("event"), "200 Meters");
        assert_eq!(result.get::<String,_>("raw_mark"), "23.33a");
        assert_eq!(result.get::<String,_>("meet_date"), "2025-05-01");
        assert!(result.get::<String,_>("source_url").contains("athleteId=28872883"));
        let completed: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE status='succeeded'").fetch_one(&test.pool).await?;
        assert_eq!(completed, 1);
        let discoveries: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE status='pending'").fetch_one(&test.pool).await?;
        assert!(discoveries > 0);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_saved_marks_when_invalid_replay_fails_then_checkpoint_is_not_completed() -> Result<()>
{
    with_schema(|test| Box::pin(async move {
        assert_success(&import_bio(test).await?);
        let before: Vec<(String,String)> = sqlx::query_as("SELECT event,raw_mark FROM results ORDER BY event,raw_mark").fetch_all(&test.pool).await?;
        let invalid = test.directory().join("invalid.json");
        std::fs::write(&invalid, b"not source JSON")?;
        let url = "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=999999&sport=tf&level=0";
        let output = test.operation(&crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((invalid.to_str().context("UTF-8 path")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Athlete,(url).to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),athlete_id:Some(("999999").to_string()), ..Default::default()})}).await?;
        assert!(!output.status.success());
        let after: Vec<(String,String)> = sqlx::query_as("SELECT event,raw_mark FROM results ORDER BY event,raw_mark").fetch_all(&test.pool).await?;
        assert_eq!(after,before);
        let page: (String,Option<String>) = sqlx::query_as("SELECT status,last_success::text FROM scrape_pages WHERE url=$1").bind(url).fetch_one(&test.pool).await?;
        assert_eq!(page,("failed".to_owned(),None));
        Ok(())
    })).await
}

async fn exhaust_failed_bio_replay(test: &support::Scenario) -> Result<()> {
    let invalid = test.directory().join("invalid-bio.json");
    std::fs::write(&invalid, b"not source JSON")?;
    let output = test.operation(&crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((invalid.to_str().context("UTF-8 path")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Athlete,("https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0").to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),athlete_id:Some(("28872883").to_string()),season:Some(("2026").parse::<i32>()?),sport:Some(track_scrapper_v2::model::Sport::Outdoor), ..Default::default()})}).await?;
    assert!(!output.status.success());
    sqlx::query("UPDATE scrape_pages SET attempts=3 WHERE status='failed'")
        .execute(&test.pool)
        .await?;
    Ok(())
}

#[tokio::test]
async fn given_exhausted_failure_when_valid_capture_is_replayed_then_data_is_saved_without_another_attempt()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        exhaust_failed_bio_replay(test).await?;
        assert_success(&import_bio(test).await?);
        let state: (String, i32, bool, Option<String>) = sqlx::query_as(
            "SELECT status,attempts,last_success IS NOT NULL,last_error FROM scrape_pages WHERE url LIKE '%athleteId=28872883%'",
        ).fetch_one(&test.pool).await?;
        assert_eq!(state, ("succeeded".into(), 3, true, None));
        let mark: String = sqlx::query_scalar("SELECT raw_mark FROM results WHERE source_result_id='258445410'")
            .fetch_one(&test.pool).await?;
        assert_eq!(mark, "23.33a");
        Ok(())
    })).await
}

#[tokio::test]
async fn given_exhausted_failure_when_invalid_capture_is_replayed_then_failure_and_attempt_count_remain()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        exhaust_failed_bio_replay(test).await?;
        let invalid = test.directory().join("invalid-bio.json");
        let output = test.operation(&crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((invalid.to_str().context("UTF-8 path")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Athlete,("https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0").to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),athlete_id:Some(("28872883").to_string()),season:Some(("2026").parse::<i32>()?),sport:Some(track_scrapper_v2::model::Sport::Outdoor), ..Default::default()})}).await?;
        assert!(!output.status.success());
        let state: (String, i32, bool, bool) = sqlx::query_as(
            "SELECT status,attempts,last_success IS NOT NULL,last_error IS NOT NULL FROM scrape_pages WHERE url LIKE '%athleteId=28872883%'",
        ).fetch_one(&test.pool).await?;
        assert_eq!(state, ("failed".into(), 3, false, true));
        let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(results, 0);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_failed_page_when_resumed_repeatedly_then_attempts_stop_at_three() -> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::AthleticNet, "11");
            database.enqueue(&[target], false).await?;
            for expected in 1..=3 {
                let job = database
                    .next_job(None, 3, &[])
                    .await?
                    .context("failed page should be resumable below three attempts")?;
                assert!(database.start_attempt(job.id, 3).await?);
                database
                    .fail_job(&job, None, "fixture parse failure", false)
                    .await?;
                let state: (String, i32, Option<String>) = sqlx::query_as(
                    "SELECT status,attempts,last_success::text FROM scrape_pages WHERE id=$1",
                )
                .bind(job.id)
                .fetch_one(&test.pool)
                .await?;
                assert_eq!(state, ("failed".to_owned(), expected, None));
            }
            assert!(database.next_job(None, 3, &[]).await?.is_none());
            let id: i64 = sqlx::query_scalar("SELECT id FROM scrape_pages")
                .fetch_one(&test.pool)
                .await?;
            assert!(!database.start_attempt(id, 3).await?);
            let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM scrape_pages")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(attempts, 3);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_parser_challenge_when_replayed_and_refreshed_then_block_is_preserved() -> Result<()>
{
    with_schema(|test| {
        Box::pin(async move {
            let path = test.directory().join("challenge.html");
            std::fs::write(&path, "<title>Log In - MileSplit</title>")?;
            let url = "https://oh.milesplit.com/athletes/15005189-brice-fuller";
            let args = crate::support::OfflineOperation::Replay {
                config: None,
                input: std::path::PathBuf::from((path.to_str().context("UTF-8 path")?).to_string()),
                target: track_scrapper_v2::model::Target::get(
                    track_scrapper_v2::model::Source::MileSplit,
                    track_scrapper_v2::model::PageKind::Athlete,
                    (url).to_string(),
                    track_scrapper_v2::model::PageContext {
                        state: Some(("OH").to_string()),
                        athlete_id: Some(("15005189").to_string()),
                        sport: Some(track_scrapper_v2::model::Sport::Outdoor),
                        ..Default::default()
                    },
                ),
            };
            assert!(!test.operation(&args).await?.status.success());
            let before: (String, i32, Option<String>) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE url=$1")
                    .bind(url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(before.0, "blocked");
            assert_eq!(before.1, 1);
            assert!(
                before
                    .2
                    .as_deref()
                    .is_some_and(|error| error.contains("access denied"))
            );
            let valid = fixture("milesplit/athlete-oh-15005189.html");
            let mut replay = args;
            if let support::OfflineOperation::Replay { input, .. } = &mut replay {
                *input = valid;
            }
            assert!(!test.operation(&replay).await?.status.success());
            let after: (String, i32, Option<String>) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE url=$1")
                    .bind(url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(after, before);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_exhausted_transient_checkpoint_when_durable_scope_starts_then_original_count_and_replayed_reservation_are_preserved()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            complete_seed_pages(&database).await?;
            let mut page = target(Source::AthleticNet, "11");
            page.context.state = Some("WI".to_owned());
            database.enqueue(&[page.clone()], false).await?;
            sqlx::query(
                "UPDATE scrape_pages SET status='failed', attempts=3, last_error='network failure' WHERE url LIKE '%/athlete/11'",
            )
            .execute(&test.pool)
            .await?;
            let config=track_scrapper_v2::config::Config::default();
            let scope=page_support::scope(&config)?;
            let request=track_scrapper_v2::durable::page::bind_target(&database,page,&scope).await?;
            let now=chrono::Utc::now().timestamp_millis();
            let first=page_support::reserve(&database,&request,now).await?;
            assert_eq!((first.total_attempts,first.cycle,first.ordinal),(4,1,1));
            let replay=page_support::reserve(&database,&request,now+1).await?;
            assert_eq!(serde_json::to_value(&replay)?,serde_json::to_value(&first)?);
            let original:i32=sqlx::query_scalar("SELECT original_attempts FROM durable_page_operations WHERE operation_id=$1")
                .bind(&request.binding.operation_id).fetch_one(&test.pool).await?;
            assert_eq!(original,3);
            let state: (String, i32, Option<String>) = sqlx::query_as(
                "SELECT status,attempts,last_error FROM scrape_pages WHERE id=$1",
            )
            .bind(request.binding.page_id).fetch_one(&test.pool).await?;
            assert_eq!(state,("pending".to_owned(),4,Some("network failure".to_owned())));
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_attempts WHERE operation_id=$1")
                .bind(&request.binding.operation_id).fetch_one(&test.pool).await?,1);
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(&test.pool).await?,0);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_nonblocked_acquisition_failure_when_scraping_then_three_attempts_are_recorded()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            complete_seed_pages(&database).await?;
            let page = failure_target("11")?;
            database.enqueue(std::slice::from_ref(&page), false).await?;
            for expected in 1..=3 {
                let job = database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()])
                    .await?.context("retryable acquisition checkpoint")?;
                assert_eq!(job.target.url, page.url);
                missing_capture_failure(test, &job.target).await?;
                let state: (String, i32, bool, bool) = sqlx::query_as(
                    "SELECT status,attempts,last_success IS NULL,last_error LIKE 'opening replay input %' FROM scrape_pages WHERE id=$1",
                ).bind(job.id).fetch_one(&test.pool).await?;
                assert_eq!(state, ("failed".to_owned(), expected, true, true));
            }
            assert!(database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()]).await?.is_none());
            let config=track_scrapper_v2::config::Config::default();
            let scope=page_support::scope(&config)?;
            let request=track_scrapper_v2::durable::page::bind_target(&database,page.clone(),&scope).await?;
            assert!(matches!(track_scrapper_v2::durable::page::reserve(&database,&request,chrono::Utc::now().timestamp_millis()).await?,track_scrapper_v2::durable::page::ReservationDecision::Settled{outcome:track_scrapper_v2::durable::page::PageOutcome::Rejected{..}}));
            let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM scrape_pages WHERE url=$1")
                .bind(&page.url).fetch_one(&test.pool).await?;
            assert_eq!(attempts, 3);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_failed_page_when_other_page_is_pending_then_it_is_attempted_before_retry()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            complete_seed_pages(&database).await?;
            let first = failure_target("11")?;
            let second = failure_target("12")?;
            database.enqueue(&[first.clone(), second.clone()], false).await?;
            let first_job = database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()])
                .await?.context("first acquisition checkpoint")?;
            assert_eq!(first_job.target.url, first.url);
            missing_capture_failure(test, &first_job.target).await?;
            let next = database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()])
                .await?.context("untouched checkpoint before failed retry")?;
            assert_eq!(next.target.url, second.url);
            missing_capture_failure(test, &next.target).await?;
            let states: Vec<(String, i32, Option<String>)> = sqlx::query_as(
                "SELECT status,attempts,last_success::text FROM scrape_pages WHERE url=$1 OR url=$2 ORDER BY id",
            ).bind(&first.url).bind(&second.url).fetch_all(&test.pool).await?;
            assert_eq!(states, vec![("failed".into(), 1, None), ("failed".into(), 1, None)]);
            let retry = database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()])
                .await?.context("failed checkpoint remains retryable after untouched page")?;
            assert_eq!(retry.id, first_job.id);
            database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_exhausted_page_when_other_page_is_pending_then_other_page_is_drained() -> Result<()>
{
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            complete_seed_pages(&database).await?;
            let first = failure_target("11")?;
            let second = failure_target("12")?;
            database.enqueue(&[first.clone(), second.clone()], false).await?;
            for _ in 0..3 {
                missing_capture_failure(test, &first).await?;
            }
            for _ in 0..3 {
                let next = database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()])
                    .await?.context("pending page is drained despite earlier exhausted checkpoint")?;
                assert_eq!(next.target.url, second.url);
                missing_capture_failure(test, &next.target).await?;
            }
            assert!(database.next_job(Some(Source::AthleticNet), 3, &["WI".to_owned()]).await?.is_none());
            let config=track_scrapper_v2::config::Config::default();
            let scope=page_support::scope(&config)?;
            let request=track_scrapper_v2::durable::page::bind_target(&database,first.clone(),&scope).await?;
            assert!(matches!(track_scrapper_v2::durable::page::reserve(&database,&request,chrono::Utc::now().timestamp_millis()).await?,track_scrapper_v2::durable::page::ReservationDecision::Settled{outcome:track_scrapper_v2::durable::page::PageOutcome::Rejected{..}}));
            let states: Vec<(String, i32, Option<String>)> = sqlx::query_as(
                "SELECT status,attempts,last_success::text FROM scrape_pages WHERE url=$1 OR url=$2 ORDER BY id",
            ).bind(&first.url).bind(&second.url).fetch_all(&test.pool).await?;
            assert_eq!(states, vec![("failed".into(), 3, None), ("failed".into(), 3, None)]);
            database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_interrupted_attempt_when_resumed_then_count_is_preserved_and_untouched_page_first()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let first = target(Source::AthleticNet, "11");
            let second = target(Source::AthleticNet, "12");
            database
                .enqueue(&[first.clone(), second.clone()], false)
                .await?;
            let job = database
                .next_job(None, 3, &[])
                .await?
                .context("first page")?;
            assert_eq!(job.target.url, first.url);
            assert!(database.start_attempt(job.id, 3).await?);
            database.enqueue(std::slice::from_ref(&first), true).await?;
            let next = database
                .next_job(None, 3, &[])
                .await?
                .context("untouched page")?;
            assert_eq!(next.target.url, second.url);
            let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM scrape_pages WHERE id=$1")
                .bind(job.id)
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(attempts, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_blocked_page_when_scraping_then_pending_page_is_not_attempted() -> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            complete_seed_pages(&database).await?;
            let first = target(Source::AthleticNet, "11");
            let second = target(Source::AthleticNet, "12");
            database
                .enqueue(&[first.clone(), second.clone()], false)
                .await?;
            let job = database
                .next_job(Some(Source::AthleticNet), 3, &[])
                .await?
                .context("first page")?;
            assert!(database.start_attempt(job.id, 3).await?);
            database
                .fail_job(&job, None, "access challenge", true)
                .await?;
            assert!(
                track_scrapper_v2::runner::durable::bootstrap(
                    &database,
                    &track_scrapper_v2::config::Config::default()
                )
                .await
                .is_err()
            );
            let state: (String, i32) =
                sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE url=$1")
                    .bind(&second.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(state, ("pending".into(), 0));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_namesakes_when_observed_then_source_identities_are_not_name_merged() -> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for (source, id) in [
                (Source::AthleticNet, "11"),
                (Source::AthleticNet, "12"),
                (Source::MileSplit, "21"),
            ] {
                persist(
                    &database,
                    &target(source, id),
                    &ParsedPage {
                        athletes: vec![athlete(id)],
                        ..ParsedPage::default()
                    },
                )
                .await?;
            }
            let identities: Vec<(String, String)> = sqlx::query_as(
                "SELECT source,source_id FROM athlete_sources ORDER BY source,source_id",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(
                identities,
                vec![
                    ("athleticnet".to_owned(), "11".to_owned()),
                    ("athleticnet".to_owned(), "12".to_owned()),
                    ("milesplit".to_owned(), "21".to_owned())
                ]
            );
            let count: i64 =
                sqlx::query_scalar("SELECT count(DISTINCT athlete_id) FROM athlete_sources")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(count, 3);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_conflicting_linked_profiles_when_observed_then_needs_review_prevents_silent_merge()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    athletes: vec![athlete("11")],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let other = Athlete {
                graduation_year: Some(2028),
                ..athlete("21")
            };
            persist(
                &database,
                &target(Source::MileSplit, "21"),
                &ParsedPage {
                    athletes: vec![other],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let linked = Athlete {
                same_as: vec!["https://www.milesplit.com/athletes/21".to_owned()],
                ..athlete("11")
            };
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    athletes: vec![linked],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let reviews: Vec<(i64, bool)> =
                sqlx::query_as("SELECT id,needs_review FROM athletes ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(
                reviews
                    .iter()
                    .map(|(_, review)| *review)
                    .collect::<Vec<_>>(),
                vec![true, true]
            );
            database.close().await;
            let output = test.directory().join("review.xlsx");
            assert_success(
                &test
                    .operation(&crate::support::OfflineOperation::Export {
                        config: None,
                        output: std::path::PathBuf::from(
                            (output.to_str().context("UTF-8 output path")?).to_string(),
                        ),
                        per_athlete: false,
                    })
                    .await?,
            );
            let rows = sheet_rows(&output, "Athletes")?;
            let headers = rows.first().context("missing athlete headers")?;
            let id_column = headers
                .iter()
                .position(|header| header == "athlete_id")
                .context("missing canonical athlete ID")?;
            let review_column = headers
                .iter()
                .position(|header| header == "needs_review")
                .context("missing review flag")?;
            let mut written = std::collections::BTreeSet::new();
            for values in rows.iter().skip(1) {
                assert!(
                    written.insert(values.get(id_column).context("missing athlete ID")?.clone())
                );
                assert_eq!(
                    values.get(review_column).context("missing review flag")?,
                    "true"
                );
            }
            assert_eq!(
                written,
                reviews.iter().map(|(id, _)| id.to_string()).collect()
            );
            assert_eq!(sheet_rows(&output, "Issues")?.len(), 3);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_corrected_result_when_refreshed_then_raw_current_mark_updates_without_duplicate()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::AthleticNet, "11");
            let original = ParsedPage {
                athletes: vec![athlete("11")],
                performances: vec![performance("11.25")],
                ..ParsedPage::default()
            };
            persist(&database, &target, &original).await?;
            persist(
                &database,
                &target,
                &ParsedPage {
                    performances: vec![performance("11.15")],
                    ..original
                },
            )
            .await?;
            let results: Vec<(String, String)> =
                sqlx::query_as("SELECT raw_mark,source_url FROM results")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(results, vec![("11.15".to_owned(), target.url)]);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_native_milesplit_profile_when_replayed_then_all_sports_retain_published_prs()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let path = fixture("milesplit/athlete-oh-15005189.html");
        let args = crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((path.to_str().context("UTF-8 fixture path")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::MileSplit,track_scrapper_v2::model::PageKind::Athlete,("https://oh.milesplit.com/athletes/15005189-brice-fuller").to_string(),track_scrapper_v2::model::PageContext {state:Some(("OH").to_string()),athlete_id:Some(("15005189").to_string()),sport:Some(track_scrapper_v2::model::Sport::Outdoor), ..Default::default()})};
        assert_success(&test.operation(&args).await?);
        assert_success(&test.operation(&args).await?);
        let profiles: Vec<(String,String,Option<i32>)> = sqlx::query_as("SELECT source_id,name,graduation_year FROM athlete_sources").fetch_all(&test.pool).await?;
        assert_eq!(profiles,vec![("15005189".to_owned(),"Brice Fuller".to_owned(),Some(2028))]);
        for (sport,event,expected) in [("outdoor","1600m",vec!["4:12.69","4:33.74"]),("indoor","3000m",vec!["8:26.83"]),("xc","5K",vec!["14:49.06"])] {
            let marks: Vec<String> = sqlx::query_scalar("SELECT DISTINCT raw_mark FROM results WHERE sport=$1 AND event=$2 ORDER BY raw_mark").bind(sport).bind(event).fetch_all(&test.pool).await?;
            assert_eq!(marks,expected,"{sport} {event}");
        }
        let dates: Vec<(String,String)> = sqlx::query_as("SELECT DISTINCT raw_mark,meet_date FROM results WHERE sport='outdoor' AND event='1600m' ORDER BY raw_mark").fetch_all(&test.pool).await?;
        assert_eq!(dates,vec![("4:12.69".to_owned(),"2026-06-04".to_owned()),("4:33.74".to_owned(),"2024-05-11".to_owned())]);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_school_and_unknown_level_career_when_exported_then_observations_survive_without_hs_pr()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let school = fixture("athleticnet/team-core-3204.json");
        assert_success(&test.operation(&crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((school.to_str().context("UTF-8 path")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Team,("https://www.athletic.net/api/v1/TeamHome/GetTeamCore?teamId=3204&sport=tfo&year=2026").to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),school_id:Some(("3204").to_string()), ..Default::default()})}).await?);
        assert_success(&import_bio(test).await?);
        let database = Database::connect(&test.url).await?;
        let published = Athlete {
            name: "Kingston Penn".to_owned(),
            graduation_year: Some(2027),
            ..athlete("28872883")
        };
        persist(&database, &target(Source::AthleticNet, "28872883"), &ParsedPage {
            athletes: vec![published],
            ..ParsedPage::default()
        }).await?;
        database.close().await;
        let output = test.directory().join("athletes.xlsx");
        assert_success(&test.operation(&crate::support::OfflineOperation::Export {config:None,output:std::path::PathBuf::from((output.to_str().context("UTF-8 path")?).to_string()),per_athlete:true}).await?);
        let rows = sheet_rows(&output,"Athletes")?;
        assert_eq!(rows.len(),2);
        assert_eq!(cell(&rows,"name")?,"Kingston Penn");
        assert_eq!(cell(&rows,"school_names")?,"Middleton");
        assert!(cell(&rows,"school_postal_addresses")?.contains("2100 Bristol St"));
        assert!(cell(&rows,"school_postal_addresses")?.contains("53562-2699"));
        assert!(rows[0].iter().all(|header| !header.starts_with("best_observed::athleticnet::")));
        let observations = sheet_rows(&output, "Observations")?;
        assert!(observations.iter().skip(1).any(|row| {
            row.iter().any(|cell| cell.contains("10.41a"))
                && row.iter().any(|cell| cell.contains("athleteId=28872883"))
        }));
        assert!(sheet_rows(&output, "Issues")?.iter().skip(1).any(|row| {
            row.iter().any(|cell| cell.contains("high-school") || cell.contains("HS"))
        }));
        let original = std::fs::read(&output)?;
        let duplicate = test.operation(&crate::support::OfflineOperation::Export {config:None,output:std::path::PathBuf::from((output.to_str().context("UTF-8 path")?).to_string()),per_athlete:false}).await?;
        assert!(!duplicate.status.success(),"existing workbook must not be overwritten");
        assert_eq!(std::fs::read(&output)?,original);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_real_public_staff_when_replayed_then_only_published_coaching_mailboxes_are_stored()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let path = fixture("contacts/miramonte-staff.html");
            let database = Database::connect(&test.url).await?;
            let school = track_scrapper_v2::model::School {
                id: "miramonte".to_owned(),
                name: "Miramonte High School".to_owned(),
                state: Some("CA".to_owned()),
                ..track_scrapper_v2::model::School::default()
            };
            let member = Athlete {
                school_id: Some("miramonte".to_owned()),
                state: Some("CA".to_owned()),
                ..athlete("11")
            };
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    schools: vec![school],
                    athletes: vec![member],
                    ..ParsedPage::default()
                },
            )
            .await?;
            database.close().await;
            assert_success(
                &test
                    .operation(&crate::support::OfflineOperation::Replay {
                        config: None,
                        input: std::path::PathBuf::from(
                            (path.to_str().context("UTF-8 path")?).to_string(),
                        ),
                        target: track_scrapper_v2::model::Target::get(
                            track_scrapper_v2::model::Source::AthleticNet,
                            track_scrapper_v2::model::PageKind::Contacts,
                            ("https://gomats.org/staff-directory").to_string(),
                            track_scrapper_v2::model::PageContext {
                                state: Some(("CA").to_string()),
                                school_id: Some(("miramonte").to_string()),
                                school_name: Some(("Miramonte High School").to_string()),
                                ..Default::default()
                            },
                        ),
                    })
                    .await?,
            );
            let contacts: Vec<(String, Option<String>, String)> = sqlx::query_as(
                "SELECT email,sport,source_url FROM contacts WHERE kind='coach' ORDER BY email",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(
                contacts,
                vec![
                    (
                        "bhenderson@auhsdschools.org".to_owned(),
                        Some("xc".to_owned()),
                        "https://gomats.org/staff-directory".to_owned()
                    ),
                    (
                        "caljumper259@gmail.com".to_owned(),
                        None,
                        "https://gomats.org/staff-directory".to_owned()
                    ),
                    (
                        "dharris@auhsdschools.org".to_owned(),
                        None,
                        "https://gomats.org/staff-directory".to_owned()
                    )
                ]
            );
            let output = test.directory().join("contacts.xlsx");
            assert_success(
                &test
                    .operation(&crate::support::OfflineOperation::Export {
                        config: None,
                        output: std::path::PathBuf::from(
                            (output.to_str().context("UTF-8 output path")?).to_string(),
                        ),
                        per_athlete: false,
                    })
                    .await?,
            );
            let rows = sheet_rows(&output, "Athletes")?;
            assert_eq!(rows.len(), 2);
            let emails = cell(&rows, "coach_emails")?;
            for email in [
                "bhenderson@auhsdschools.org",
                "caljumper259@gmail.com",
                "dharris@auhsdschools.org",
            ] {
                assert!(emails.contains(email), "{email}");
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_compatible_explicit_link_when_observed_then_two_sources_share_one_athlete()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    athletes: vec![athlete("11")],
                    ..ParsedPage::default()
                },
            )
            .await?;
            persist(
                &database,
                &target(Source::MileSplit, "21"),
                &ParsedPage {
                    athletes: vec![athlete("21")],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let linked = Athlete {
                same_as: vec!["https://www.milesplit.com/athletes/21".to_owned()],
                ..athlete("11")
            };
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    athletes: vec![linked],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let identities: i64 =
                sqlx::query_scalar("SELECT count(DISTINCT athlete_id) FROM athlete_sources")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(identities, 1);
            let reviews: Vec<bool> = sqlx::query_scalar("SELECT needs_review FROM athletes")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(reviews, vec![false]);
            let records = database.athlete_records().await?;
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].name, "Alex Morgan");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_unknown_graduation_year_when_exported_then_source_row_is_visible_and_flagged()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let unknown = Athlete {
                graduation_year: None,
                ..athlete("11")
            };
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    athletes: vec![unknown],
                    ..ParsedPage::default()
                },
            )
            .await?;
            database.close().await;
            let output = test.directory().join("unknown-year.xlsx");
            assert_success(
                &test
                    .operation(&crate::support::OfflineOperation::Export {
                        config: None,
                        output: std::path::PathBuf::from(
                            (output.to_str().context("UTF-8 output path")?).to_string(),
                        ),
                        per_athlete: false,
                    })
                    .await?,
            );
            let athletes = sheet_rows(&output, "Athletes")?;
            assert_eq!(athletes.len(), 2);
            assert_eq!(cell(&athletes, "name")?, "Alex Morgan");
            assert_eq!(cell(&athletes, "graduation_year")?, "");
            assert_eq!(cell(&athletes, "needs_review")?, "true");
            assert!(cell(&athletes, "review_notes")?.contains("unknown graduation year"));
            let issues = sheet_rows(&output, "Issues")?;
            assert_eq!(cell(&issues, "name")?, "Alex Morgan");
            assert!(cell(&issues, "issues")?.contains("unknown graduation year"));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_invalid_result_reference_when_persistence_fails_then_identity_and_page_success_roll_back()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let invalid = track_scrapper_v2::model::Performance {
                athlete_id: "missing".to_owned(),
                ..performance("11.25")
            };
            let page = ParsedPage {
                athletes: vec![athlete("11")],
                performances: vec![invalid],
                ..ParsedPage::default()
            };
            let error = persist(&database, &target(Source::AthleticNet, "11"), &page)
                .await
                .err()
                .context("invalid result reference unexpectedly persisted")?;
            assert!(format!("{error:#}").contains("missing"));
            let athletes: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(&test.pool)
                .await?;
            let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
                .fetch_one(&test.pool)
                .await?;
            let successes: i64 =
                sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE status='succeeded'")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!((athletes, results, successes), (0, 0, 0));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_school_scoped_office_email_when_contacts_replay_then_school_mailbox_is_not_a_coach()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let school = track_scrapper_v2::model::School {
                id: "example".to_owned(),
                name: "Example High School".to_owned(),
                state: Some("CA".to_owned()),
                ..track_scrapper_v2::model::School::default()
            };
            let member = Athlete {
                school_id: Some("example".to_owned()),
                state: Some("CA".to_owned()),
                ..athlete("11")
            };
            persist(
                &database,
                &target(Source::AthleticNet, "11"),
                &ParsedPage {
                    schools: vec![school],
                    athletes: vec![member],
                    ..ParsedPage::default()
                },
            )
            .await?;
            database.close().await;
            let path = fixture("contacts/scoped-school.html");
            assert_success(
                &test
                    .operation(&crate::support::OfflineOperation::Replay {
                        config: None,
                        input: std::path::PathBuf::from(
                            (path.to_str().context("UTF-8 fixture path")?).to_string(),
                        ),
                        target: track_scrapper_v2::model::Target::get(
                            track_scrapper_v2::model::Source::AthleticNet,
                            track_scrapper_v2::model::PageKind::Contacts,
                            ("https://school.example.edu/athletics/staff").to_string(),
                            track_scrapper_v2::model::PageContext {
                                state: Some(("CA").to_string()),
                                school_id: Some(("example").to_string()),
                                school_name: Some(("Example High School").to_string()),
                                ..Default::default()
                            },
                        ),
                    })
                    .await?,
            );
            let school: (String, String, String) = sqlx::query_as(
                "SELECT address,postal_code,public_email FROM schools WHERE source_id='example'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                school,
                (
                    "123 School Road".to_owned(),
                    "90210".to_owned(),
                    "office@school.example.edu".to_owned()
                )
            );
            let coaches: Vec<String> =
                sqlx::query_scalar("SELECT email FROM contacts WHERE kind='coach' ORDER BY email")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(
                coaches,
                vec![
                    "ada.runner@school.example.edu",
                    "ben.walker@school.example.edu",
                    "chris.thrower@school.example.edu"
                ]
            );
            let output = test.directory().join("office.xlsx");
            assert_success(
                &test
                    .operation(&crate::support::OfflineOperation::Export {
                        config: None,
                        output: std::path::PathBuf::from(
                            (output.to_str().context("UTF-8 export path")?).to_string(),
                        ),
                        per_athlete: false,
                    })
                    .await?,
            );
            let rows = sheet_rows(&output, "Athletes")?;
            assert_eq!(
                cell(&rows, "school_public_emails")?,
                "office@school.example.edu"
            );
            assert!(!cell(&rows, "coach_emails")?.contains("office@"));
            assert!(!cell(&rows, "school_public_emails")?.contains("district@"));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_native_unlinked_results_when_replayed_then_rows_survive_without_invented_athletes()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let path = fixture("milesplit/oh_meet_770621_rs1321880_raw.html");
        let url = "https://oh.milesplit.com/meets/770621-beaver-eastern-invite-2026/results/1321880/raw";
        let args = crate::support::OfflineOperation::Replay {config:None,input:std::path::PathBuf::from((path.to_str().context("UTF-8 fixture path")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::MileSplit,track_scrapper_v2::model::PageKind::Results,(url).to_string(),track_scrapper_v2::model::PageContext {state:Some(("OH").to_string()),sport:Some(track_scrapper_v2::model::Sport::CrossCountry), ..Default::default()})};
        assert_success(&test.operation(&args).await?);
        assert_success(&test.operation(&args).await?);
        let unassigned: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE athlete_id IS NULL AND athlete_source_id IS NULL")
            .fetch_one(&test.pool).await?;
        assert_eq!(unassigned,80);
        let athletes: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes").fetch_one(&test.pool).await?;
        assert_eq!(athletes,0);
        let first: (String,String,String,String) = sqlx::query_as(
            "SELECT event,raw_mark,source_url,raw::text FROM results WHERE raw::text LIKE '%Jeydyn Fields%'")
            .fetch_one(&test.pool).await?;
        assert_eq!(first.0,"Boys Middle School 3000 Meter");
        assert_eq!(first.1,"12:40.6");
        assert_eq!(first.2,url);
        assert!(first.3.contains("Jeydyn Fields"));
        Ok(())
    })).await
}

#[tokio::test]
async fn given_changed_source_facts_when_reimported_then_originals_and_review_flag_survive()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target(Source::AthleticNet, "11");
            let mut first = athlete("11");
            first.gender = Some("female".to_owned());
            first.grade = Some("10".to_owned());
            persist(
                &database,
                &target,
                &ParsedPage {
                    athletes: vec![first.clone()],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let mut changed = first;
            changed.name = "Different Name".to_owned();
            changed.gender = Some("male".to_owned());
            changed.state = Some("OR".to_owned());
            changed.graduation_year = Some(2028);
            changed.grade = Some("11".to_owned());
            persist(
                &database,
                &target,
                &ParsedPage {
                    athletes: vec![changed],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let stored: (String, String, String, i32, String, bool) = sqlx::query_as(
                "SELECT p.name,p.gender,p.state,p.graduation_year,p.grade,a.needs_review \
             FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                stored,
                (
                    "Alex Morgan".to_owned(),
                    "female".to_owned(),
                    "WI".to_owned(),
                    2027,
                    "11".to_owned(),
                    true
                )
            );
            let canonical: (String, String, String, i32) =
                sqlx::query_as("SELECT name,gender,state,graduation_year FROM athletes")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                canonical,
                (
                    "Alex Morgan".to_owned(),
                    "female".to_owned(),
                    "WI".to_owned(),
                    2027
                )
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
