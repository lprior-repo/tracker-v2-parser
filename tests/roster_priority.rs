mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, JobClass},
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
        content_type: if body.trim_start().starts_with('<') {
            "text/html"
        } else {
            "application/json"
        }
        .into(),
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
async fn given_all_classes_when_each_source_completes_two_cycles_then_profiles_rosters_and_bulk_progress()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for source in [Source::AthleticNet, Source::MileSplit] {
                let targets: Vec<_> = (0..12)
                    .map(|id| target(source, PageKind::Athlete, id))
                    .chain((100..102).map(|id| target(source, PageKind::Roster, id)))
                    .chain((200..202).map(|id| target(source, PageKind::Directory, id)))
                    .collect();
                database.enqueue(&targets, false).await?;
            }
            for _ in 0..2 {
                for (priority, expected) in [
                    (JobClass::Profile, PageKind::Athlete),
                    (JobClass::Profile, PageKind::Athlete),
                    (JobClass::Profile, PageKind::Athlete),
                    (JobClass::Roster, PageKind::Roster),
                    (JobClass::Profile, PageKind::Athlete),
                    (JobClass::Profile, PageKind::Athlete),
                    (JobClass::Profile, PageKind::Athlete),
                    (JobClass::Bulk, PageKind::Directory),
                ] {
                    for source in [Source::AthleticNet, Source::MileSplit] {
                        let job = database
                            .next_job_with_priority(Some(source), 3, priority)
                            .await?
                            .context("pending source work")?;
                        assert_eq!(job.target.source, source);
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
                }
            }
            assert!(database.next_job(None, 3).await?.is_none());
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn exercise_retry_limit(
    database: &Database,
    source: Source,
    limit: u32,
    priority: JobClass,
    id: usize,
) -> Result<()> {
    database
        .enqueue(&[target(source, PageKind::Athlete, id)], false)
        .await?;
    let failed = database
        .next_job(Some(source), limit)
        .await?
        .context("athlete")?;
    assert!(database.start_attempt(failed.id, limit).await?);
    database.fail_job(&failed, None, "failure", false).await?;
    database
        .enqueue(&[target(source, PageKind::Team, id + 1)], false)
        .await?;
    let untouched = database
        .next_job_with_priority(Some(source), limit, priority)
        .await?
        .context("untouched work precedes failed profile")?;
    assert_eq!(untouched.target.kind, PageKind::Team);
    assert!(database.start_attempt(untouched.id, limit).await?);
    database.fail_job(&untouched, None, "blocked", true).await?;
    for _ in 1..limit {
        let retry = database
            .next_job_with_priority(Some(source), limit, priority)
            .await?
            .context("retry")?;
        assert_eq!(retry.id, failed.id);
        assert!(database.start_attempt(retry.id, limit).await?);
        database.fail_job(&retry, None, "failure", false).await?;
    }
    assert!(
        database
            .next_job_with_priority(Some(source), limit, priority)
            .await?
            .is_none()
    );
    assert!(!database.start_attempt(failed.id, limit).await?);
    assert!(!database.start_attempt(untouched.id, limit).await?);
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM scrape_pages WHERE id=$1")
        .bind(failed.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(attempts, i32::try_from(limit)?);
    save(
        database,
        Some(failed.id),
        &failed.target,
        "{}",
        &ParsedPage::default(),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn given_failed_priority_when_untouched_work_exists_then_retry_is_bounded_and_blocked_is_excluded()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let mut id = 0;
            for source in [Source::AthleticNet, Source::MileSplit] {
                for limit in 1..=3 {
                    for priority in [JobClass::Profile, JobClass::Roster, JobClass::Bulk] {
                        exercise_retry_limit(&database, source, limit, priority, id).await?;
                        id += 2;
                    }
                }
            }
            for invalid in [0, 4] {
                assert!(database.next_job(None, invalid).await.is_err());
                assert!(database.start_attempt(1, invalid).await.is_err());
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_old_roster_backlog_when_later_milesplit_profile_is_selected_then_its_public_prs_are_saved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let rosters: Vec<_> = (0..100)
                .map(|id| target(Source::MileSplit, PageKind::Roster, id))
                .collect();
            database.enqueue(&rosters, false).await?;
            let athlete = sources::milesplit::athlete_target("15005189", Some("OH"), Sport::Outdoor)?;
            database.enqueue(std::slice::from_ref(&athlete), false).await?;
            let job = database.next_job(None, 3).await?.context("later athlete")?;
            assert_eq!(job.target.source, Source::MileSplit);
            assert_eq!(job.target.kind, PageKind::Athlete);
            assert_eq!(job.target.url, athlete.url);
            assert_eq!(job.target.context.athlete_id.as_deref(), Some("15005189"));
            let older_rosters: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM scrape_pages WHERE id < $1 AND cursor->>'kind'='roster' AND attempts=0",
            )
            .bind(job.id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(older_rosters, 100);
            assert!(database.start_attempt(job.id, 3).await?);
            let body = include_str!("../fixtures/milesplit/athlete-oh-15005189.html");
            let page = sources::parse(&job.target, body)?;
            save(&database, Some(job.id), &job.target, body, &page).await?;
            let records: Vec<(String, String, String, String, String)> = sqlx::query_as(
                "SELECT p.source_id,r.sport,r.event,r.raw_mark,r.raw->'source_values'->>'level' FROM results r \
                 JOIN athlete_sources p ON p.id=r.athlete_source_id \
                 WHERE p.source='milesplit' AND r.is_personal_record \
                 AND ((r.sport='outdoor' AND r.event='1600m') \
                 OR (r.sport='indoor' AND r.event='3000m') OR (r.sport='xc' AND r.event='5K')) \
                 ORDER BY r.sport,r.event,r.raw->'source_values'->>'level'",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(
                records,
                vec![
                    ("15005189".into(), "indoor".into(), "3000m".into(), "8:26.83".into(), "all".into()),
                    ("15005189".into(), "indoor".into(), "3000m".into(), "8:26.83".into(), "hs".into()),
                    ("15005189".into(), "outdoor".into(), "1600m".into(), "4:12.69".into(), "all".into()),
                    ("15005189".into(), "outdoor".into(), "1600m".into(), "4:12.69".into(), "hs".into()),
                    ("15005189".into(), "outdoor".into(), "1600m".into(), "4:33.74".into(), "ms".into()),
                    ("15005189".into(), "xc".into(), "5K".into(), "14:49.06".into(), "all".into()),
                    ("15005189".into(), "xc".into(), "5K".into(), "14:49.06".into(), "hs".into()),
                ],
            );
            let status: String = sqlx::query_scalar("SELECT status FROM scrape_pages WHERE id=$1")
                .bind(job.id).fetch_one(&test.pool).await?;
            assert_eq!(status, "succeeded");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_missing_preferred_classes_when_selecting_then_remaining_classes_follow_profile_first_order()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for source in [Source::AthleticNet, Source::MileSplit] {
                for (index, priority, order) in [
                    (
                        0,
                        JobClass::Profile,
                        [PageKind::Athlete, PageKind::Roster, PageKind::Directory],
                    ),
                    (
                        1,
                        JobClass::Roster,
                        [PageKind::Roster, PageKind::Athlete, PageKind::Directory],
                    ),
                    (
                        2,
                        JobClass::Bulk,
                        [PageKind::Directory, PageKind::Athlete, PageKind::Roster],
                    ),
                ] {
                    let targets = [
                        target(source, PageKind::Directory, index * 3),
                        target(source, PageKind::Roster, index * 3 + 1),
                        target(source, PageKind::Athlete, index * 3 + 2),
                    ];
                    database.enqueue(&targets, false).await?;
                    for expected in order {
                        let job = database
                            .next_job_with_priority(Some(source), 3, priority)
                            .await?
                            .context("remaining class")?;
                        assert_eq!(job.target.source, source);
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
                    assert!(database.next_job(Some(source), 3).await?.is_none());
                }
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}
