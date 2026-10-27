mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    database::{Database, JobClass},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
    sources,
};

fn target(source: Source, kind: PageKind, id: &str, state: Option<&str>) -> Target {
    let host = match source {
        Source::AthleticNet => "www.athletic.net",
        Source::MileSplit => "www.milesplit.com",
    };
    Target::get(
        source,
        kind,
        format!("https://{host}/fixture/{id}"),
        PageContext {
            state: state.map(str::to_owned),
            ..PageContext::default()
        },
    )
}

async fn complete(database: &Database, job_id: Option<i64>, target: &Target) -> Result<()> {
    database
        .persist(
            job_id,
            target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".into(),
                body: b"{}".to_vec(),
            },
            &ParsedPage::default(),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn given_older_national_backlog_when_wisconsin_is_selected_then_only_wisconsin_consumes_attempts()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let states = ["WI".to_owned()];
            for source in [Source::AthleticNet, Source::MileSplit] {
                let outside = target(source, PageKind::Athlete, "older-alabama", Some("AL"));
                let unknown = target(source, PageKind::Athlete, "unknown-scope", None);
                database.enqueue(&[outside.clone(), unknown], false).await?;
                let selected = [
                    (PageKind::Athlete, "wisconsin-athlete"),
                    (PageKind::Roster, "wisconsin-roster"),
                    (PageKind::Contacts, "wisconsin-contacts"),
                    (PageKind::Team, "wisconsin-team"),
                ]
                .map(|(kind, id)| target(source, kind, id, Some("WI")));
                database.enqueue(&selected, false).await?;
                for preferred in [
                    JobClass::Profile,
                    JobClass::Roster,
                    JobClass::Contacts,
                    JobClass::Bulk,
                ] {
                    let job = database
                        .next_job_with_priority(Some(source), 3, preferred, &states)
                        .await?
                        .context("Wisconsin job")?;
                    assert_eq!(job.target.context.state.as_deref(), Some("WI"));
                    assert!(database.start_attempt(job.id, 3).await?);
                    complete(&database, Some(job.id), &job.target).await?;
                }
                assert!(database.next_job(Some(source), 3, &states).await?.is_none());
                let untouched: Vec<(String, i32)> = sqlx::query_as(
                    "SELECT status,attempts FROM scrape_pages WHERE source=$1 AND cursor->'context'->>'state' IS DISTINCT FROM 'WI' ORDER BY id",
                )
                .bind(source.as_str())
                .fetch_all(&test.pool)
                .await?;
                assert_eq!(untouched, vec![("pending".into(), 0), ("pending".into(), 0)]);
                assert_eq!(
                    database.next_job(Some(source), 3, &[]).await?.context("preserved national backlog")?.target.url,
                    outside.url
                );
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_national_and_wisconsin_bootstraps_when_scoped_then_only_exact_wisconsin_roots_run()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let states = ["WI".to_owned()];
            let broad = sources::athleticnet::seeds(&["AL".into(), "WI".into()], 2026)?;
            let narrow = sources::athleticnet::seeds(&states, 2026)?;
            database.enqueue(&broad, false).await?;
            database.enqueue(&narrow, false).await?;
            for expected in &narrow {
                let job = database
                    .next_job(Some(Source::AthleticNet), 3, &states)
                    .await?
                    .context("scoped bootstrap")?;
                assert_eq!(job.target.url, expected.url);
                assert_eq!(job.target.context.subject_id.as_deref(), Some("WI"));
                assert!(database.start_attempt(job.id, 3).await?);
                complete(&database, Some(job.id), &job.target).await?;
            }
            assert!(database.next_job(Some(Source::AthleticNet), 3, &states).await?.is_none());
            let untouched: Vec<(String, i32)> = sqlx::query_as(
                "SELECT status,attempts FROM scrape_pages WHERE cursor->'context'->>'subject_id'='AL,WI' ORDER BY id",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(untouched, vec![("pending".into(), 0); broad.len()]);
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn wisconsin_config() -> &'static str {
    "states=['WI']\ngraduation_years=[2027]\nseason=2026\n[sources.athleticnet]\nenabled=true\ntransport='http'\n[sources.milesplit]\nenabled=false\ntransport='http'\n"
}

#[tokio::test]
async fn given_outside_state_failures_when_wisconsin_drains_then_cli_succeeds_without_consuming_them()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = Config::parse(wisconsin_config())?;
            for seed in sources::athleticnet::seeds(&config.states, config.season)? {
                complete(&database, None, &seed).await?;
            }
            let mut pending = sources::athleticnet::athlete_target("11", Some("AL"), Sport::Outdoor)?;
            pending.token_url = Some("https://www.athletic.net:444/preflight".into());
            let exhausted = sources::athleticnet::athlete_target("12", Some("AL"), Sport::Outdoor)?;
            let mut unknown = sources::athleticnet::athlete_target("13", None, Sport::Outdoor)?;
            unknown.token_url.clone_from(&pending.token_url);
            database.enqueue(&[pending, exhausted.clone(), unknown], false).await?;
            sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='retained outside-state failure' WHERE url=$1")
                .bind(&exhausted.url).execute(&test.pool).await?;
            let before: Vec<(i64, String, i32, Option<String>)> = sqlx::query_as(
                "SELECT id,status,attempts,last_error FROM scrape_pages ORDER BY id",
            ).fetch_all(&test.pool).await?;
            let path = test.directory().join("wisconsin.toml");
            std::fs::write(&path, wisconsin_config())?;
            let output = test.cli(&["scrape", "--config", path.to_str().context("UTF8 configuration path")?]).await?;
            support::assert_success(&output);
            let after: Vec<(i64, String, i32, Option<String>)> = sqlx::query_as(
                "SELECT id,status,attempts,last_error FROM scrape_pages ORDER BY id",
            ).fetch_all(&test.pool).await?;
            assert_eq!(after, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_provider_access_stop_outside_wisconsin_when_scope_changes_then_human_review_is_still_required()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let config = Config::parse(wisconsin_config())?;
            for seed in sources::athleticnet::seeds(&config.states, config.season)? {
                complete(&database, None, &seed).await?;
            }
            let blocked = sources::athleticnet::athlete_target("14", Some("AL"), Sport::Outdoor)?;
            database.enqueue(&[blocked], false).await?;
            let job = database
                .next_job(None, 3, &[])
                .await?
                .context("outside-state blocked job")?;
            assert!(database.start_attempt(job.id, 3).await?);
            database
                .fail_job(
                    &job,
                    None,
                    "access challenge requiring human handling",
                    true,
                )
                .await?;
            let path = test.directory().join("wisconsin.toml");
            std::fs::write(&path, wisconsin_config())?;
            let output = test
                .cli(&[
                    "scrape",
                    "--config",
                    path.to_str().context("UTF8 configuration path")?,
                ])
                .await?;
            assert!(!output.status.success());
            let state: (String, i32, String) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE id=$1")
                    .bind(job.id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                state,
                (
                    "blocked".into(),
                    1,
                    "access challenge requiring human handling".into()
                )
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[test]
fn given_wisconsin_profile_when_meets_are_discovered_then_continuations_keep_discovery_scope()
-> Result<()> {
    let target = sources::athleticnet::athlete_target("28872883", Some("WI"), Sport::Outdoor)?;
    let page = sources::parse(
        &target,
        include_str!("../fixtures/athleticnet/bio-28872883.json"),
    )?;
    let meets: Vec<_> = page
        .next
        .iter()
        .filter(|target| target.kind == PageKind::Meet)
        .collect();
    assert_eq!(
        meets
            .iter()
            .map(|target| target.context.state.as_deref())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([Some("WI")])
    );
    let mut meet = sources::athleticnet::meet_target("634313", Sport::Outdoor)?;
    meet.context.state = Some("WI".into());
    let metadata = sources::parse(
        &meet,
        include_str!("../fixtures/athleticnet/meet-634313.json"),
    )?;
    let results = metadata.next.first().context("discovered meet results")?;
    assert_eq!(results.kind, PageKind::Results);
    assert_eq!(results.context.state.as_deref(), Some("WI"));
    assert_eq!(results.context.subject_id.as_deref(), Some("634313"));
    Ok(())
}
