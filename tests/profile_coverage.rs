mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, Job},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
};

fn root(id: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://wi.milesplit.com/athletes/{id}-published"),
        PageContext {
            state: Some("WI".into()),
            athlete_id: Some(id.into()),
            ..PageContext::default()
        },
    )
}

async fn register(test: &support::Scenario, id: &str, year: Option<i32>) -> Result<Target> {
    let target = root(id);
    let athlete: i64 = sqlx::query_scalar(
        "INSERT INTO athletes(name,graduation_year) VALUES('Published Runner',$1) RETURNING id",
    )
    .bind(year)
    .fetch_one(&test.pool)
    .await?;
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,graduation_year) VALUES($1,'milesplit',$2,$3,$4)")
        .bind(athlete).bind(id).bind(&target.url).bind(year).execute(&test.pool).await?;
    Ok(target)
}

async fn next(database: &Database) -> Result<Option<Job>> {
    database
        .next_profile_coverage_job(Some(Source::MileSplit), 3, &["WI".into()], &[2027], false)
        .await
}

async fn snapshot(test: &support::Scenario, except: Option<i64>) -> Result<Vec<serde_json::Value>> {
    Ok(sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM scrape_pages p WHERE $1::bigint IS NULL OR id<>$1 ORDER BY id",
    )
    .bind(except)
    .fetch_all(&test.pool)
    .await?)
}

async fn complete(database: &Database, job: Option<i64>, target: &Target) -> Result<()> {
    database
        .persist(
            job,
            target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: b"<html></html>".to_vec(),
            },
            &ParsedPage::default(),
        )
        .await?;
    Ok(())
}

async fn finish(database: &Database, job: &Job) -> Result<()> {
    assert!(database.start_attempt(job.id, 3).await?);
    complete(database, Some(job.id), &job.target).await
}

async fn irrelevant(test: &support::Scenario, selected: &Target) -> Result<Vec<Target>> {
    let earlier_cohort = register(test, "12", Some(2026)).await?;
    let outside_year = register(test, "13", Some(2028)).await?;
    let mut outside_state = register(test, "14", Some(2027)).await?;
    outside_state.context.state = Some("AL".into());
    let mut wrong_identity = root("15");
    wrong_identity.context.athlete_id = None;
    let mut targets = vec![earlier_cohort, outside_year, outside_state, wrong_identity];
    for suffix in ["stats", "progression", "articles", "photos", "videos"] {
        let mut target = selected.clone();
        target.url.push('/');
        target.url.push_str(suffix);
        targets.push(target);
    }
    for kind in [
        PageKind::Directory,
        PageKind::Team,
        PageKind::Roster,
        PageKind::Meet,
        PageKind::Results,
        PageKind::Contacts,
    ] {
        let mut target = selected.clone();
        target.kind = kind;
        targets.push(target);
    }
    Ok(targets)
}

#[tokio::test]
async fn given_known_selected_root_and_other_work_when_coverage_is_selected_then_only_base_and_public_result_followups_run()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let selected = register(test, "11", Some(2027)).await?;
            let skipped = irrelevant(test, &selected).await?;
            database.enqueue(&skipped, false).await?;
            database
                .enqueue(std::slice::from_ref(&selected), false)
                .await?;
            let before = snapshot(test, None).await?;
            let job = next(&database).await?.context("known published base")?;
            assert_eq!(job.target.url, selected.url);
            assert_eq!(snapshot(test, None).await?, before);
            let changed = [
                selected.url.clone(),
                format!("{}/stats", selected.url),
                format!("{}/progression", selected.url),
            ];
            let untouched: Vec<serde_json::Value> = sqlx::query_scalar(
                "SELECT to_jsonb(p) FROM scrape_pages p WHERE NOT(url=ANY($1)) ORDER BY id",
            )
            .bind(&changed)
            .fetch_all(&test.pool)
            .await?;
            finish(&database, &job).await?;
            for suffix in ["stats", "progression"] {
                let followup = next(&database).await?.context("public results followup")?;
                assert_eq!(followup.target.url, format!("{}/{suffix}", selected.url));
                finish(&database, &followup).await?;
            }
            assert!(next(&database).await?.is_none());
            let after: Vec<serde_json::Value> = sqlx::query_scalar(
                "SELECT to_jsonb(p) FROM scrape_pages p WHERE NOT(url=ANY($1)) ORDER BY id",
            )
            .bind(&changed)
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(after, untouched);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_successful_alias_and_context_duplicates_when_resumed_then_each_source_identity_is_fetched_once_and_followup_success_does_not_hide_base()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let selected = register(test, "21", Some(2027)).await?;
            let mut duplicate = selected.clone();
            duplicate.context.sport = Some(Sport::Outdoor);
            let fetched_alias = register(test, "22", Some(2027)).await?;
            let mut older_alias = fetched_alias.clone();
            older_alias.url = "https://www.milesplit.com/athletes/22-old-published-slug/".into();
            complete(&database, None, &older_alias).await?;
            let last = register(test, "23", Some(2027)).await?;
            let mut stats = last.clone();
            stats.url.push_str("/stats");
            complete(&database, None, &stats).await?;
            let jobs = [selected.clone(), duplicate, fetched_alias, last.clone()];
            database.enqueue(&jobs, false).await?;
            for expected in [last, selected] {
                let job = next(&database)
                    .await?
                    .context("next unseen base identity")?;
                assert_eq!(job.target.url, expected.url);
                finish(&database, &job).await?;
            }
            let before = snapshot(test, None).await?;
            assert!(next(&database).await?.is_none());
            database.enqueue(&jobs, false).await?;
            assert!(next(&database).await?.is_none());
            assert_eq!(snapshot(test, None).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_exhausted_base_and_irrelevant_failures_when_coverage_is_checked_then_only_unfinished_selected_base_blocks_completion()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let selected = register(test, "31", Some(2027)).await?;
        let skipped = irrelevant(test, &selected).await?;
        database.enqueue(&skipped, false).await?;
        database.enqueue(std::slice::from_ref(&selected), false).await?;
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='retained exhausted checkpoint'").execute(&test.pool).await?;
        let before = snapshot(test, None).await?;
        assert!(next(&database).await?.is_none());
        let error = database.check_profile_coverage_exhausted(Source::MileSplit, 3, &["WI".into()], &[2027], false).await.expect_err("selected exhausted root");
        assert!(error.to_string().contains("exhausted unfinished school-chain"));
        assert_eq!(snapshot(test, None).await?, before);
        complete(&database, None, &selected).await?;
        for suffix in ["stats", "progression"] {
            let mut followup = selected.clone(); followup.url.push('/'); followup.url.push_str(suffix);
            complete(&database, None, &followup).await?;
        }
        database.check_profile_coverage_exhausted(Source::MileSplit, 3, &["WI".into()], &[2027], false).await?;
        assert!(database.next_profile_coverage_job(None, 0, &["WI".into()], &[2027], false).await.is_err());
        assert!(database.check_profile_coverage_exhausted(Source::MileSplit, 4, &["WI".into()], &[2027], false).await.is_err());
        database.close().await; Ok(())
    })).await
}

#[tokio::test]
async fn given_nonpublished_alias_or_wrong_provider_when_coverage_is_selected_then_no_url_is_guessed_or_reused()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let selected = register(test, "41", Some(2027)).await?;
            let mut alias = selected.clone();
            alias.url = "https://wi.milesplit.com/athletes/41-unpublished-alias".into();
            let mut query = selected.clone();
            query.url.push_str("?page=2");
            database.enqueue(&[alias, query], false).await?;
            let before = snapshot(test, None).await?;
            assert!(next(&database).await?.is_none());
            assert!(
                database
                    .next_profile_coverage_job(
                        Some(Source::AthleticNet),
                        3,
                        &["WI".into()],
                        &[2027],
                        false,
                    )
                    .await?
                    .is_none()
            );
            assert_eq!(snapshot(test, None).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn focus_config(test: &support::Scenario) -> Result<std::path::PathBuf> {
    let path = test.directory().join("profile-coverage.toml");
    std::fs::write(
        &path,
        "states=['WI']\ngraduation_years=[2027]\nseason=2026\n[acquisition]\nmode='public_results'\nprofile_coverage_only=true\ncollect_contacts=true\n[limits]\nmax_pages=10\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\nenabled=true\ntransport='http'\n",
    )?;
    Ok(path)
}

#[tokio::test]
async fn given_only_irrelevant_pending_work_when_focused_cli_runs_then_no_discovery_contacts_followups_or_refresh_are_attempted()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let selected = register(test, "61", Some(2027)).await?;
            database
                .enqueue(&irrelevant(test, &selected).await?, false)
                .await?;
            let before = snapshot(test, None).await?;
            let path = focus_config(test)?;
            let path = path.to_str().context("UTF8 focus config")?;
            let output = test.cli(&["scrape", "--config", path]).await?;
            support::assert_success(&output);
            assert_eq!(snapshot(test, None).await?, before);
            let refreshed = test.cli(&["scrape", "--config", path, "--refresh"]).await?;
            assert!(!refreshed.status.success());
            assert!(String::from_utf8_lossy(&refreshed.stderr).contains("refresh"));
            assert_eq!(snapshot(test, None).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_existing_provider_stop_when_focused_cli_has_no_known_base_work_then_human_review_still_blocks_it()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let blocked = register(test, "71", Some(2028)).await?;
        database.enqueue(std::slice::from_ref(&blocked), false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=1,last_error='HTTP 403 challenge requiring human review'").execute(&test.pool).await?;
        let before = snapshot(test, None).await?;
        let path = focus_config(test)?;
        let output = test.cli(&["scrape", "--config", path.to_str().context("UTF8 focus config")?]).await?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("human review"));
        assert_eq!(snapshot(test, None).await?, before);
        database.close().await; Ok(())
    })).await
}
