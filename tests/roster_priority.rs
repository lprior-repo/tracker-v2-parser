mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
    sources,
};

fn target(source: Source, kind: PageKind, id: usize) -> Target {
    let host = match source {
        Source::AthleticNet => "www.athletic.net",
        Source::MileSplit => "www.milesplit.com",
    };
    Target::get(
        source,
        kind,
        format!("https://{host}/fixture/{id}"),
        PageContext::default(),
    )
}

async fn save(
    database: &Database,
    id: Option<i64>,
    target: &Target,
    body: &str,
    page: &ParsedPage,
) -> Result<()> {
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".into(),
        body: body.as_bytes().to_vec(),
    };
    database.persist(id, target, &capture, page).await?;
    Ok(())
}

#[tokio::test]
async fn given_late_roster_amid_bulk_when_selected_then_fixture_athletes_are_saved() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let bulk: Vec<_> = (0..100).map(|id| target(Source::AthleticNet, PageKind::Directory, id)).collect();
        database.enqueue(&bulk, false).await?;
        let core = sources::athleticnet::team_target("3204", Some("WI"), Sport::Outdoor, 2026)?;
        let body = include_str!("../fixtures/athleticnet/team-core-3204.json");
        let page = sources::parse(&core, body)?;
        save(&database, None, &core, body, &page).await?;
        let job = database.next_job(Some(Source::AthleticNet), 3).await?.context("roster should be selected")?;
        assert_eq!(job.target.kind, PageKind::Roster);
        assert!(database.start_attempt(job.id, 3).await?);
        let body = include_str!("../fixtures/athleticnet/roster-3204.json");
        let page = sources::parse(&job.target, body)?;
        save(&database, Some(job.id), &job.target, body, &page).await?;
        let athlete: (String, String, Option<i32>) = sqlx::query_as("SELECT source_id,name,graduation_year FROM athlete_sources WHERE source_id='25401378'")
            .fetch_one(&test.pool).await?;
        assert_eq!(athlete, ("25401378".into(), "Abby Arnold".into(), None));
        assert_eq!(database.next_job(Some(Source::AthleticNet), 3).await?.context("profile should be selected")?.target.kind, PageKind::Athlete);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_both_classes_when_work_continues_then_bulk_and_each_source_progress() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut targets: Vec<_> = (0..6)
                .map(|id| target(Source::AthleticNet, PageKind::Athlete, id))
                .collect();
            targets.push(target(Source::AthleticNet, PageKind::Directory, 100));
            targets.push(target(Source::MileSplit, PageKind::Roster, 200));
            database.enqueue(&targets, false).await?;
            for (slot, expected) in [
                PageKind::Athlete,
                PageKind::Athlete,
                PageKind::Athlete,
                PageKind::Directory,
            ]
            .into_iter()
            .enumerate()
            {
                let job = database
                    .next_job_with_priority(Some(Source::AthleticNet), 3, slot == 3)
                    .await?
                    .context("pending work")?;
                assert_eq!(job.target.kind, expected);
                assert!(database.start_attempt(job.id, 3).await?);
                save(
                    &database,
                    Some(job.id),
                    &job.target,
                    "{}",
                    &ParsedPage::default(),
                )
                .await?;
            }
            let other = database
                .next_job(Some(Source::MileSplit), 3)
                .await?
                .context("other source work")?;
            assert_eq!(other.target.kind, PageKind::Roster);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_failed_priority_when_untouched_work_exists_then_retry_is_bounded_and_blocked_is_excluded()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let athlete = target(Source::AthleticNet, PageKind::Athlete, 1);
            database.enqueue(&[athlete], false).await?;
            let failed = database
                .next_job(Some(Source::AthleticNet), 3)
                .await?
                .context("athlete")?;
            assert!(database.start_attempt(failed.id, 3).await?);
            database.fail_job(&failed, None, "failure", false).await?;
            let bulk = target(Source::AthleticNet, PageKind::Team, 2);
            database.enqueue(&[bulk], false).await?;
            let untouched = database
                .next_job(Some(Source::AthleticNet), 3)
                .await?
                .context("untouched")?;
            assert_eq!(untouched.target.kind, PageKind::Team);
            assert!(database.start_attempt(untouched.id, 3).await?);
            database.fail_job(&untouched, None, "blocked", true).await?;
            for _ in 0..2 {
                let retry = database
                    .next_job(Some(Source::AthleticNet), 3)
                    .await?
                    .context("retry")?;
                assert_eq!(retry.id, failed.id);
                assert!(database.start_attempt(retry.id, 3).await?);
                database.fail_job(&retry, None, "failure", false).await?;
            }
            assert!(
                database
                    .next_job(Some(Source::AthleticNet), 3)
                    .await?
                    .is_none()
            );
            assert!(!database.start_attempt(failed.id, 3).await?);
            database.close().await;
            Ok(())
        })
    })
    .await
}
