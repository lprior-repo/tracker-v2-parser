mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, Job, JobClass},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Target},
};

fn target(source: Source, id: &str, kind: PageKind) -> Target {
    let host = match source {
        Source::AthleticNet => "www.athletic.net",
        Source::MileSplit => "wi.milesplit.com",
    };
    Target::get(
        source,
        kind,
        format!("https://{host}/fixture/{id}"),
        PageContext {
            state: Some("WI".to_owned()),
            athlete_id: Some(id.to_owned()),
            ..PageContext::default()
        },
    )
}

async fn register(
    test: &support::Scenario,
    source: Source,
    id: &str,
    canonical_year: Option<i32>,
    published_year: Option<i32>,
) -> Result<()> {
    let athlete: i64 = sqlx::query_scalar(
        "INSERT INTO athletes(name,graduation_year) VALUES('Same published name',$1) RETURNING id",
    )
    .bind(canonical_year)
    .fetch_one(&test.pool)
    .await?;
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,graduation_year) VALUES($1,$2,$3,$4)")
        .bind(athlete).bind(source.as_str()).bind(id).bind(published_year)
        .execute(&test.pool).await?;
    Ok(())
}

async fn finish(database: &Database, job: &Job) -> Result<()> {
    assert!(database.start_attempt(job.id, 3).await?);
    database
        .persist(
            Some(job.id),
            &job.target,
            &Capture {
                url: job.target.url.clone(),
                status: 200,
                content_type: "application/json".to_owned(),
                body: b"{}".to_vec(),
            },
            &ParsedPage::default(),
        )
        .await?;
    Ok(())
}

async fn snapshot(
    test: &support::Scenario,
    source: Source,
    ids: &[&str],
) -> Result<Vec<serde_json::Value>> {
    Ok(sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE source=$1 AND cursor->'context'->>'athlete_id'=ANY($2) ORDER BY id")
        .bind(source.as_str()).bind(ids).fetch_all(&test.pool).await?)
}

fn discovery_classes() -> [PageKind; 6] {
    [
        PageKind::Directory,
        PageKind::Team,
        PageKind::Roster,
        PageKind::Meet,
        PageKind::Results,
        PageKind::Contacts,
    ]
}

async fn next(database: &Database, source: Source, preferred: JobClass) -> Result<Option<Job>> {
    database
        .next_job_for_cohort(Some(source), 3, preferred, &["WI".into()], &[2027])
        .await
}

async fn prepare_profiles(
    test: &support::Scenario,
    database: &Database,
    source: Source,
) -> Result<()> {
    for (id, canonical, published) in [
        ("11", Some(2028), Some(2028)),
        ("12", Some(2026), Some(2026)),
        ("13", Some(2027), Some(2027)),
        ("14", None, None),
        ("17", Some(2028), None),
    ] {
        register(test, source, id, canonical, published).await?;
    }
    let jobs =
        ["11", "12", "13", "14", "15", "16", "17"].map(|id| target(source, id, PageKind::Athlete));
    database.enqueue(&jobs, false).await?;
    sqlx::query("UPDATE scrape_pages SET status='failed',attempts=2,last_error='preserved transient failure',notes=ARRAY['original checkpoint note'] WHERE source=$1 AND cursor->'context'->>'athlete_id'='12'")
        .bind(source.as_str()).execute(&test.pool).await?;
    Ok(())
}

#[tokio::test]
async fn given_known_other_years_when_cohort_jobs_are_selected_then_unknown_and_unreserved_profiles_run_without_changing_skipped_checkpoints()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for source in [Source::AthleticNet, Source::MileSplit] {
                prepare_profiles(test, &database, source).await?;
                let before = snapshot(test, source, &["11", "12", "17"]).await?;
                for expected in ["13", "14", "15", "16"] {
                    let job = next(&database, source, JobClass::Profile)
                        .await?
                        .context("eligible cohort profile")?;
                    assert_eq!(job.target.context.athlete_id.as_deref(), Some(expected));
                    finish(&database, &job).await?;
                }
                assert!(next(&database, source, JobClass::Profile).await?.is_none());
                assert_eq!(snapshot(test, source, &["11", "12", "17"]).await?, before);
                assert_eq!(
                    database
                        .next_job_with_priority(Some(source), 3, JobClass::Profile, &["WI".into()])
                        .await?
                        .context("legacy unfiltered queue")?
                        .target
                        .context
                        .athlete_id
                        .as_deref(),
                    Some("11")
                );
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_known_other_year_identity_when_nonprofile_work_is_queued_then_every_discovery_and_contact_class_still_runs()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            register(test, Source::MileSplit, "21", Some(2028), Some(2028)).await?;
            let kinds = discovery_classes();
            let jobs = kinds.map(|kind| target(Source::MileSplit, "21", kind));
            database.enqueue(&jobs, false).await?;
            let mut seen = Vec::new();
            for _ in kinds {
                let job = next(&database, Source::MileSplit, JobClass::Profile)
                    .await?
                    .context("discovery class remains eligible")?;
                seen.push(job.target.kind);
                finish(&database, &job).await?;
            }
            assert_eq!(
                seen,
                vec![
                    PageKind::Roster,
                    PageKind::Contacts,
                    PageKind::Directory,
                    PageKind::Team,
                    PageKind::Meet,
                    PageKind::Results
                ]
            );
            assert!(
                next(&database, Source::MileSplit, JobClass::Profile)
                    .await?
                    .is_none()
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_unknown_profile_identity_or_selected_published_year_when_cohort_is_selected_then_source_ids_control_selection()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            register(test, Source::AthleticNet, "31", Some(2028), Some(2028)).await?;
            register(test, Source::MileSplit, "32", Some(2028), Some(2027)).await?;
            register(test, Source::MileSplit, "33", Some(2028), Some(2028)).await?;
            let mut missing_identity = target(Source::MileSplit, "33", PageKind::Athlete);
            missing_identity.context.athlete_id = None;
            let jobs = [
                target(Source::MileSplit, "31", PageKind::Athlete),
                target(Source::MileSplit, "32", PageKind::Athlete),
                missing_identity,
            ];
            database.enqueue(&jobs, false).await?;
            for expected in jobs {
                let job = next(&database, Source::MileSplit, JobClass::Profile)
                    .await?
                    .context("unknown or selected source identity")?;
                assert_eq!(job.target.url, expected.url);
                finish(&database, &job).await?;
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_cohort_jobs_when_state_attempt_and_contact_stops_apply_then_only_the_eligible_priority_job_runs()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let mut outside = target(Source::MileSplit, "41", PageKind::Athlete);
        outside.context.state = Some("AL".into());
        let exhausted = target(Source::MileSplit, "42", PageKind::Athlete);
        let contact = target(Source::MileSplit, "43", PageKind::Contacts);
        let blocked = target(Source::MileSplit, "44", PageKind::Contacts);
        let roster = target(Source::MileSplit, "45", PageKind::Roster);
        let profile = target(Source::MileSplit, "46", PageKind::Athlete);
        database.enqueue(&[outside, exhausted.clone(), contact, blocked.clone(), profile, roster.clone()], false).await?;
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3 WHERE url=$1").bind(&exhausted.url).execute(&test.pool).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=1,last_error='HTTP 403 access stop' WHERE url=$1").bind(&blocked.url).execute(&test.pool).await?;
        let before: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id").fetch_all(&test.pool).await?;
        let job = next(&database, Source::MileSplit, JobClass::Roster).await?.context("priority roster")?;
        assert_eq!(job.target.url, roster.url);
        let after: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p ORDER BY id").fetch_all(&test.pool).await?;
        assert_eq!(after, before);
        finish(&database, &job).await?;
        let job = next(&database, Source::MileSplit, JobClass::Contacts).await?.context("profile because contacts are stopped")?;
        assert_eq!(job.target.kind, PageKind::Athlete);
        finish(&database, &job).await?;
        assert!(next(&database, Source::MileSplit, JobClass::Contacts).await?.is_none());
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_only_exhausted_other_year_profiles_when_cohort_crawl_finishes_then_cli_succeeds_and_preserves_them()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        register(test, Source::MileSplit, "51", Some(2028), Some(2028)).await?;
        let profile = target(Source::MileSplit, "51", PageKind::Athlete);
        database.enqueue(std::slice::from_ref(&profile), false).await?;
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='preserved exhausted other-year profile' WHERE url=$1")
            .bind(&profile.url).execute(&test.pool).await?;
        for seed in track_scrapper_v2::sources::milesplit::seeds(&["WI".into()], 2026)? {
            database.persist(None, &seed, &Capture { url: seed.url.clone(), status: 200, content_type: "text/html".into(), body: b"<html></html>".to_vec() }, &ParsedPage::default()).await?;
        }
        let before = snapshot(test, Source::MileSplit, &["51"]).await?;
        let path = test.directory().join("cohort.toml");
        std::fs::write(&path, "states=['WI']\ngraduation_years=[2027]\nseason=2026\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\nenabled=true\ntransport='http'\n")?;
        let output = test.cli(&["scrape", "--config", path.to_str().context("UTF8 config path")?]).await?;
        support::assert_success(&output);
        assert_eq!(snapshot(test, Source::MileSplit, &["51"]).await?, before);
        database.close().await;
        Ok(())
    })).await
}
