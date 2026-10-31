use super::{RetryPolicy, test_support::*, wire_tests::connection};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ReservationDecision},
    },
};
use anyhow::Result;

#[tokio::test]
async fn three_real_http_failures_exhaust_budget_without_a_fourth_request_after_cooldown()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(3)?;
            let mut now = chrono::Utc::now().timestamp_millis();
            let scope = scope(&config, now)?;
            let request = page::bind_target(&database, target(3), &scope).await?;
            let root = tempfile::tempdir()?;
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 503,
                body: "temporary failure",
            })
            .await?;
            let mut transport = connection(root.path(), &fixture, &database)?;
            let mut last = None;
            for attempt in 1..=3 {
                let ReservationDecision::Reserved { reservation } =
                    page::reserve_configured(&database, &config, &request, now).await?
                else {
                    anyhow::bail!("reservation {attempt}")
                };
                let FetchDecision::Settled { outcome } = super::fetch_configured(
                    &database,
                    root.path(),
                    &reservation,
                    &mut transport,
                    now,
                    &config,
                )
                .await?
                else {
                    anyhow::bail!("failure {attempt}")
                };
                if attempt < 3 {
                    let PageOutcome::Deferred { retry_at_ms, .. } = outcome else {
                        anyhow::bail!("retry {attempt}")
                    };
                    now = retry_at_ms;
                } else {
                    assert!(
                        matches!(outcome,PageOutcome::Exhausted {detail} if detail.contains("3/3"))
                    );
                    last = Some(reservation);
                }
            }
            let before = snapshot(&database).await?;
            assert!(matches!(
                page::reserve_configured(
                    &database,
                    &config,
                    &request,
                    now + scope.cooldown_ms * 10
                )
                .await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            let reservation = last.ok_or_else(|| anyhow::anyhow!("last reservation"))?;
            assert!(matches!(
                super::fetch_configured(
                    &database,
                    root.path(),
                    &reservation,
                    &mut transport,
                    now + scope.cooldown_ms * 10,
                    &config
                )
                .await?,
                FetchDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            assert_eq!(snapshot(&database).await?, before);
            transport.close().await?;
            assert_eq!(fixture.finish().await?.len(), 3);
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages")
                    .fetch_one(pool)
                    .await?,
                3
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                3
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn successful_third_attempt_persists_and_replays_without_a_fourth_acquisition() -> Result<()>
{
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(3)?;
            let mut now = chrono::Utc::now().timestamp_millis();
            let request =
                page::bind_target(&database, target(15_005_189), &scope(&config, now)?).await?;
            let policy = RetryPolicy::configured(&config, &request)?;
            for _ in 0..2 {
                let ReservationDecision::Reserved { reservation } =
                    page::reserve_configured(&database, &config, &request, now).await?
                else {
                    anyhow::bail!("initial reservation")
                };
                let PageOutcome::Deferred { retry_at_ms, .. } = database
                    .durable_fail_with_policy(&reservation, failure(now), &policy)
                    .await?
                else {
                    anyhow::bail!("initial retry")
                };
                now = retry_at_ms;
            }
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &request, now).await?
            else {
                anyhow::bail!("third reservation")
            };
            assert_eq!(reservation.total_attempts, 3);
            let root = tempfile::tempdir()?;
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 200,
                body: include_str!("../../../../fixtures/milesplit/athlete-oh-15005189.html"),
            })
            .await?;
            let mut transport = connection(root.path(), &fixture, &database)?;
            let FetchDecision::Captured { capture } = super::fetch_configured(
                &database,
                root.path(),
                &reservation,
                &mut transport,
                now,
                &config,
            )
            .await?
            else {
                anyhow::bail!("third capture")
            };
            let page::ParseDecision::Parsed { parsed } =
                page::parse(&database, root.path(), &reservation, &capture, now).await?
            else {
                anyhow::bail!("third parse")
            };
            let outcome =
                page::persist(&database, root.path(), &reservation, &capture, &parsed).await?;
            assert!(matches!(outcome, PageOutcome::Completed { .. }));
            let before = snapshot(&database).await?;
            assert!(matches!(
                page::reserve_configured(&database, &config, &request, now + 60_000).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Completed { .. }
                }
            ));
            assert_eq!(
                page::persist(&database, root.path(), &reservation, &capture, &parsed).await?,
                outcome
            );
            assert_eq!(snapshot(&database).await?, before);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts")
                    .fetch_one(pool)
                    .await?,
                1
            );
            transport.close().await?;
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}
