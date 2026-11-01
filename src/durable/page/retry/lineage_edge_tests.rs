use super::{
    lineage_tests::{bound, configured, fail, history, source_target},
    test_support::*,
};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::with_database,
        page::{self, PageOutcome, ReservationDecision},
    },
};
use anyhow::{Context, Result};

#[tokio::test]
async fn given_partial_sibling_lineage_when_reserved_then_missing_ancestor_rejects_without_mutation()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = configured()?;
            let now = chrono::Utc::now().timestamp_millis();
            database.enqueue(&[source_target()], false).await?;
            fail(
                &database,
                &config,
                &bound(&config, now, "ancestor-x-schools", &[])?,
                now,
            )
            .await?;
            fail(
                &database,
                &config,
                &bound(&config, now, "ancestor-y-results", &[])?,
                now + 10_000,
            )
            .await?;
            fail(
                &database,
                &config,
                &bound(
                    &config,
                    now,
                    "sibling-a-schools",
                    &["ancestor-x", "ancestor-y"],
                )?,
                now + 20_000,
            )
            .await?;
            for parents in [["ancestor-x"], ["sibling-a"]] {
                let partial = bound(&config, now, "sibling-b-schools", &parents)?;
                let selected = page::select(&database, &config, &partial, now + 60_000)
                    .await?
                    .context("partial or unflattened sibling")?;
                let before = snapshot(&database).await?;
                let error = page::reserve_configured(&database, &config, &selected, now + 60_000)
                    .await
                    .expect_err("incomplete lineage must fail");
                assert!(error.to_string().contains("omits a retained ancestor"));
                assert_eq!(snapshot(&database).await?, before);
            }
            let full = bound(
                &config,
                now,
                "sibling-b-schools",
                &["ancestor-x", "ancestor-y"],
            )?;
            let full = page::select(&database, &config, &full, now + 60_000)
                .await?
                .context("complete sibling lineage")?;
            assert!(matches!(
                page::reserve_configured(&database, &config, &full, now + 60_000).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Exhausted { .. }
                }
            ));
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
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
async fn given_exact_campaign_labels_when_prefix_neighbor_exists_then_only_declared_families_count()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = configured()?;
            let now = chrono::Utc::now().timestamp_millis();
            history(&database, &config, now, 2).await?;
            fail(
                &database,
                &config,
                &bound(&config, now, "old-mixed-shadow-schools", &[])?,
                now + 30_000,
            )
            .await?;
            let scope = bound(&config, now, "new-iowa-schools", &["old-mixed"])?;
            let selected = page::select(&database, &config, &scope, now + 60_000)
                .await?
                .context("exact inherited family")?;
            let ReservationDecision::Reserved { reservation } =
                page::reserve_configured(&database, &config, &selected, now + 60_000).await?
            else {
                anyhow::bail!("unrelated prefix must not exhaust budget")
            };
            assert_eq!(reservation.total_attempts, 4);
            assert_eq!(database.durable_acquisition_attempts(&selected).await?, 3);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_identical_inherited_requests_when_reservations_race_then_one_attempt_is_replayed()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let config = configured()?;
            let now = chrono::Utc::now().timestamp_millis();
            history(&database, &config, now, 2).await?;
            let scope = bound(&config, now, "new-iowa-schools", &["old-mixed"])?;
            let selected = page::select(&database, &config, &scope, now + 60_000)
                .await?
                .context("remaining attempt")?;
            let (left, right) = tokio::join!(
                page::reserve_configured(&database, &config, &selected, now + 60_000),
                page::reserve_configured(&database, &config, &selected, now + 60_000)
            );
            let (
                ReservationDecision::Reserved { reservation: left },
                ReservationDecision::Reserved { reservation: right },
            ) = (left?, right?)
            else {
                anyhow::bail!("both calls must receive the same attempt")
            };
            assert_eq!(left.attempt_id, right.attempt_id);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(pool)
                    .await?,
                3
            );
            Ok(())
        })
    })
    .await
}
