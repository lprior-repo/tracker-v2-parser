use super::{RetryPolicy, test_support::*};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{self, FetchDecision, PageOutcome, ReservationDecision},
    },
    transport::{Mode, Options, Transport},
};
use anyhow::Result;

#[tokio::test]
async fn configured_retry_binding_mismatch_has_no_database_or_http_effects() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let now = chrono::Utc::now().timestamp_millis();
            let configured = config(1)?;
            let request =
                page::bind_target(&database, target(1), &scope(&configured, now)?).await?;
            let policy = RetryPolicy::configured(&configured, &request)?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &configured, &request, now).await?
            else {
                anyhow::bail!("reservation missing")
            };
            let other = config(2)?;
            let other_request =
                page::bind_target(&database, target(2), &scope(&other, now)?).await?;
            let before = snapshot(&database).await?;
            assert!(RetryPolicy::configured(&other, &request).is_err());
            assert!(
                page::reserve_configured(&database, &other, &request, now)
                    .await
                    .is_err()
            );
            assert!(
                database
                    .durable_reserve_configured(&other_request, now, None, None, &policy)
                    .await
                    .is_err()
            );
            let other_policy = RetryPolicy::configured(&other, &other_request)?;
            assert!(
                database
                    .durable_fail_with_policy(&reservation, failure(now), &other_policy)
                    .await
                    .is_err()
            );
            let root = tempfile::tempdir()?;
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 503,
                body: "temporary failure",
            })
            .await?;
            let mut transport = connection(root.path(), &fixture, &database)?;
            assert!(
                super::fetch_configured(
                    &database,
                    root.path(),
                    &reservation,
                    &mut transport,
                    now,
                    &other
                )
                .await
                .is_err()
            );
            transport.close().await?;
            assert_eq!(fixture.finish().await?.len(), 0);
            assert_eq!(snapshot(&database).await?, before);
            Ok(())
        })
    })
    .await
}

pub(super) fn connection(
    root: &std::path::Path,
    fixture: &WireFixture,
    database: &Database,
) -> Result<Transport> {
    Ok(Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: root.join("unused"),
        headless: true,
        rps: 3.0,
    })?
    .with_persistent_pacing(database.pool().clone())
    .with_http_fixture("https://wi.milesplit.com", fixture.url.clone())?)
}

#[tokio::test]
async fn configured_fetch_failure_exhausts_total_limit_and_public_library_keeps_three() -> Result<()>
{
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = config(1)?;
            let now = chrono::Utc::now().timestamp_millis();
            let scope = scope(&config, now)?;
            let request = page::bind_target(&database, target(1), &scope).await?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &request, now).await?
            else {
                anyhow::bail!("reservation missing")
            };
            let root = tempfile::tempdir()?;
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 503,
                body: "temporary failure",
            })
            .await?;
            let mut transport = connection(root.path(), &fixture, &database)?;
            let FetchDecision::Settled {
                outcome: PageOutcome::Exhausted { detail },
            } = super::fetch_configured(
                &database,
                root.path(),
                &reservation,
                &mut transport,
                now,
                &config,
            )
            .await?
            else {
                anyhow::bail!("configured deferral missing")
            };
            assert!(detail.contains("acquisition attempt budget exhausted (1/1)"));
            transport.close().await?;
            assert_eq!(fixture.finish().await?.len(), 1);
            let request = page::bind_target(&database, target(2), &scope).await?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &request, now).await?
            else {
                anyhow::bail!("public reservation missing")
            };
            let PageOutcome::Deferred { retry_at_ms, .. } =
                database.durable_fail(&reservation, failure(now)).await?
            else {
                anyhow::bail!("public deferral missing")
            };
            assert_eq!(retry_at_ms, now + 1000);
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &request, retry_at_ms).await?
            else {
                anyhow::bail!("public second attempt missing")
            };
            assert_eq!(
                (
                    reservation.cycle,
                    reservation.ordinal,
                    reservation.total_attempts
                ),
                (1, 2, 2)
            );
            Ok(())
        })
    })
    .await
}
