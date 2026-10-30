mod evidence;
mod support;
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ReservationDecision},
    },
    model::{Capture, PageContext, PageKind, Source, Target},
};
use anyhow::Result;
use support::*;

#[tokio::test]
async fn public_scope_excludes_generated_api_and_forbidden_token_before_attempt_and_prefetched_wire()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config()?;
            let now = chrono::Utc::now().timestamp_millis();
            let root = tempfile::tempdir()?;
            let api = api()?;
            let mut token = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                "https://oh.milesplit.com/athletes/123-runner".into(),
                PageContext {
                    state: Some("OH".into()),
                    athlete_id: Some("123".into()),
                    ..Default::default()
                },
            );
            token.token_url = Some(api.url.clone());
            for (index, target) in [api, token].into_iter().enumerate() {
                let request = page::bind_target(
                    &database,
                    target.clone(),
                    &scope(&config, &format!("new-exclusion-{index}"), now)?,
                )
                .await?;
                let before = checkpoint(&database, request.binding.page_id).await?;
                assert!(matches!(
                    page::reserve_configured(&database, &config, &request, now).await?,
                    ReservationDecision::Settled {
                        outcome: PageOutcome::Rejected { .. }
                    }
                ));
                assert_eq!(
                    checkpoint(&database, request.binding.page_id).await?,
                    before
                );
                let settled = snapshot(&database).await?;
                assert!(matches!(
                    page::reserve_configured(&database, &config, &request, now).await?,
                    ReservationDecision::Settled {
                        outcome: PageOutcome::Rejected { .. }
                    }
                ));
                assert_eq!(snapshot(&database).await?, settled);
                let request = page::bind_target(
                    &database,
                    target,
                    &scope(&config, &format!("prefetched-{index}"), now)?,
                )
                .await?;
                let ReservationDecision::Reserved { reservation } =
                    page::reserve(&database, &request, now).await?
                else {
                    anyhow::bail!("prefetched reservation")
                };
                let before = checkpoint(&database, request.binding.page_id).await?;
                let fixture = WireFixture::start(FixtureKind::Page {
                    status: 403,
                    body: "must remain unused",
                })
                .await?;
                let mut transport = transport(root.path(), &fixture, &database)?;
                assert!(matches!(
                    page::fetch_configured(
                        &database,
                        root.path(),
                        &reservation,
                        &mut transport,
                        now,
                        &config
                    )
                    .await?,
                    FetchDecision::Settled {
                        outcome: PageOutcome::Rejected { .. }
                    }
                ));
                assert_eq!(
                    checkpoint(&database, request.binding.page_id).await?,
                    before
                );
                let (retained_capture, retained_parse, outcome) =
                    database.durable_stages(&reservation).await?;
                assert_eq!(retained_capture, None);
                assert_eq!(retained_parse, None);
                assert!(matches!(outcome, Some(PageOutcome::Rejected { .. })));
                let settled = snapshot(&database).await?;
                assert!(matches!(
                    page::fetch_configured(
                        &database,
                        root.path(),
                        &reservation,
                        &mut transport,
                        now,
                        &config
                    )
                    .await?,
                    FetchDecision::Settled {
                        outcome: PageOutcome::Rejected { .. }
                    }
                ));
                assert_eq!(snapshot(&database).await?, settled);
                transport.close().await?;
                assert_eq!(fixture.finish().await?.len(), 0);
                assert_eq!(
                    sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages WHERE id=$1")
                        .bind(request.binding.page_id)
                        .fetch_one(pool)
                        .await?,
                    1
                );
            }
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                0
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(pool)
                    .await?,
                2
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts")
                    .fetch_one(pool)
                    .await?,
                0
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn public_scope_exclusion_never_supersedes_completed_capture_receipt() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config()?;
            let now = chrono::Utc::now().timestamp_millis();
            let root = tempfile::tempdir()?;
            let target = Target::get(
                Source::MileSplit,
                PageKind::Contacts,
                "https://school.example/staff".into(),
                PageContext {
                    state: Some("OH".into()),
                    school_id: Some("101".into()),
                    school_name: Some("School".into()),
                    ..Default::default()
                },
            );
            let scope = scope(&config, "prior-completed-excluded-scope", now)?;
            let completed = page::offline_replay(
                &database,
                root.path(),
                &scope,
                target.clone(),
                Capture {
                    url: target.url.clone(),
                    status: 200,
                    content_type: "text/html".into(),
                    body: b"<main></main>".to_vec(),
                },
            )
            .await?;
            assert!(matches!(completed, PageOutcome::Completed { .. }));
            let request = page::bind_target(&database, target, &scope).await?;
            let before = snapshot(&database).await?;
            let ReservationDecision::Settled { outcome } =
                page::reserve_configured(&database, &config, &request, now).await?
            else {
                anyhow::bail!("completed outcome")
            };
            assert_eq!(outcome, completed);
            assert_eq!(snapshot(&database).await?, before);
            Ok(())
        })
    })
    .await
}
