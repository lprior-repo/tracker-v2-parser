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
        let output = test.cli(&["scrape","--source","athleticnet","--kind","athlete","--url",url,
            "--input",invalid.to_str().context("UTF-8 path")?,"--athlete-id","999999","--state","WI"]).await?;
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
    let output = test.cli(&[
        "scrape", "--source", "athleticnet", "--kind", "athlete", "--url",
        "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0",
        "--input", invalid.to_str().context("UTF-8 path")?, "--athlete-id", "28872883",
        "--state", "WI", "--sport", "outdoor", "--season", "2026",
    ]).await?;
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
        let output = test.cli(&[
            "scrape", "--source", "athleticnet", "--kind", "athlete", "--url",
            "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0",
            "--input", invalid.to_str().context("UTF-8 path")?, "--athlete-id", "28872883",
            "--state", "WI", "--sport", "outdoor", "--season", "2026",
        ]).await?;
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
            let args = [
                "scrape",
                "--source",
                "milesplit",
                "--kind",
                "athlete",
                "--url",
                url,
                "--input",
                path.to_str().context("UTF-8 path")?,
                "--athlete-id",
                "15005189",
                "--state",
                "OH",
                "--sport",
                "outdoor",
            ];
            assert!(!test.cli(&args).await?.status.success());
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
            replay[8] = valid.to_str().context("UTF-8 fixture path")?;
            assert!(!test.cli(&replay).await?.status.success());
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
async fn given_exhausted_checkpoint_when_scraping_then_invocation_fails_without_attempt()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            complete_seed_pages(&database).await?;
            let mut page = target(Source::AthleticNet, "11");
            page.context.state = Some("WI".to_owned());
            database.enqueue(&[page], false).await?;
            sqlx::query(
                "UPDATE scrape_pages SET status='failed', attempts=3, last_error='network failure' WHERE url LIKE '%/athlete/11'",
            )
            .execute(&test.pool)
            .await?;
            let output = test.cli(&["scrape"]).await?;
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("exhausted unfinished checkpoints")
            );
            let state: (String, i32) = sqlx::query_as(
                "SELECT status,attempts FROM scrape_pages WHERE last_error='network failure'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(state, ("failed".to_owned(), 3));
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
            let mut page = target(Source::AthleticNet, "11");
            page.context.state = Some("WI".to_owned());
            page.token_url = Some("https://www.athletic.net:444/preflight".to_owned());
            database.enqueue(std::slice::from_ref(&page), false).await?;
            let output = test.cli(&["scrape"]).await?;
            assert!(!output.status.success());
            let state: (String, i32, Option<String>) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE url=$1")
                    .bind(&page.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(state.0, "failed");
            assert_eq!(state.1, 3);
            assert!(
                state
                    .2
                    .as_deref()
                    .is_some_and(|error| error.contains("unsupported port"))
            );
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
            let mut first = target(Source::AthleticNet, "11");
            let mut second = target(Source::AthleticNet, "12");
            first.context.state = Some("WI".to_owned());
            second.context.state = Some("WI".to_owned());
            first.token_url = Some("https://www.athletic.net:444/preflight".to_owned());
            second.token_url = first.token_url.clone();
            database.enqueue(&[first.clone(), second.clone()], false).await?;
            let config = test.directory().join("two-pages.toml");
            support::write_config(&config)?;
            let content = std::fs::read_to_string(&config)?.replace("max_pages = 10", "max_pages = 2");
            std::fs::write(&config, content)?;
            let output = test.cli(&["scrape", "--config", config.to_str().context("UTF-8 config")?]).await?;
            assert!(!output.status.success());
            let states: Vec<(String, i32, Option<String>)> = sqlx::query_as(
                "SELECT status,attempts,last_success::text FROM scrape_pages WHERE url=$1 OR url=$2 ORDER BY id",
            ).bind(&first.url).bind(&second.url).fetch_all(&test.pool).await?;
            assert_eq!(states, vec![("failed".into(), 1, None), ("failed".into(), 1, None)]);
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
            let mut first = target(Source::AthleticNet, "11");
            first.context.state = Some("WI".to_owned());
            let mut second = target(Source::AthleticNet, "12");
            second.context.state = Some("WI".to_owned());
            second.token_url = Some("https://www.athletic.net:444/preflight".to_owned());
            database.enqueue(&[first.clone(), second.clone()], false).await?;
            sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='previous failure' WHERE url=$1")
                .bind(&first.url).execute(&test.pool).await?;
            let output = test.cli(&["scrape"]).await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("exhausted unfinished checkpoints"));
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
            let output = test.cli(&["scrape"]).await?;
            assert!(!output.status.success());
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
                    .cli(&[
                        "export",
                        "--output",
                        output.to_str().context("UTF-8 output path")?,
                    ])
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
        let args = ["scrape","--source","milesplit","--kind","athlete","--url","https://oh.milesplit.com/athletes/15005189-brice-fuller",
            "--input",path.to_str().context("UTF-8 fixture path")?,"--athlete-id","15005189","--state","OH","--sport","outdoor"];
        assert_success(&test.cli(&args).await?);
        assert_success(&test.cli(&args).await?);
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
async fn given_school_and_career_when_exported_then_workbook_has_real_values_and_pr() -> Result<()>
{
    with_schema(|test| Box::pin(async move {
        let school = fixture("athleticnet/team-core-3204.json");
        assert_success(&test.cli(&["scrape","--source","athleticnet","--kind","team","--url",
            "https://www.athletic.net/api/v1/TeamHome/GetTeamCore?teamId=3204&sport=tfo&year=2026","--input",
            school.to_str().context("UTF-8 path")?,"--school-id","3204","--state","WI"]).await?);
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
        assert_success(&test.cli(&["export","--output",output.to_str().context("UTF-8 path")?,"--per-athlete"]).await?);
        let rows = sheet_rows(&output,"Athletes")?;
        assert_eq!(rows.len(),2);
        assert_eq!(cell(&rows,"name")?,"Kingston Penn");
        assert_eq!(cell(&rows,"school_names")?,"Middleton");
        assert!(cell(&rows,"school_postal_addresses")?.contains("2100 Bristol St"));
        assert!(cell(&rows,"school_postal_addresses")?.contains("53562-2699"));
        assert_eq!(cell(&rows,"best_observed::athleticnet::outdoor::100m::fat::unknown::mark")?,"10.41a");
        assert_eq!(cell(&rows,"best_observed::athleticnet::outdoor::100m::fat::unknown::raw_event")?,"100 Meters");
        assert!(cell(&rows,"best_observed::athleticnet::outdoor::100m::fat::unknown::source_url")?.contains("athleteId=28872883"));
        assert!(!sheet_rows(&output,"Issues")?.is_empty());
        let workbook = open_workbook_auto(&output)?;
        assert_eq!(workbook.sheet_names().len(),3);
        let original = std::fs::read(&output)?;
        let duplicate = test.cli(&["export","--output",output.to_str().context("UTF-8 path")?]).await?;
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
                    .cli(&[
                        "scrape",
                        "--source",
                        "athleticnet",
                        "--kind",
                        "contacts",
                        "--url",
                        "https://gomats.org/staff-directory",
                        "--input",
                        path.to_str().context("UTF-8 path")?,
                        "--school-id",
                        "miramonte",
                        "--school-name",
                        "Miramonte High School",
                        "--state",
                        "CA",
                    ])
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
                    .cli(&[
                        "export",
                        "--output",
                        output.to_str().context("UTF-8 output path")?,
                    ])
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
                    .cli(&[
                        "export",
                        "--output",
                        output.to_str().context("UTF-8 output path")?,
                    ])
                    .await?,
            );
            let athletes = sheet_rows(&output, "Athletes")?;
            assert_eq!(athletes.len(), 2);
            assert_eq!(cell(&athletes, "name")?, "Alex Morgan");
            assert_eq!(cell(&athletes, "graduation_year")?, "");
            assert_eq!(cell(&athletes, "needs_review")?, "true");
            assert!(cell(&athletes, "review_notes")?.contains("unknown graduation year"));
            let issues = sheet_rows(&output, "Issues")?;
            assert_eq!(issues.len(), 2);
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
                    .cli(&[
                        "scrape",
                        "--source",
                        "athleticnet",
                        "--kind",
                        "contacts",
                        "--url",
                        "https://school.example.edu/athletics/staff",
                        "--input",
                        path.to_str().context("UTF-8 fixture path")?,
                        "--school-id",
                        "example",
                        "--school-name",
                        "Example High School",
                        "--state",
                        "CA",
                    ])
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
                    .cli(&[
                        "export",
                        "--output",
                        output.to_str().context("UTF-8 export path")?,
                    ])
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
        let args = ["scrape","--source","milesplit","--kind","results","--url",url,
            "--input",path.to_str().context("UTF-8 fixture path")?,"--state","OH","--sport","xc"];
        assert_success(&test.cli(&args).await?);
        assert_success(&test.cli(&args).await?);
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
