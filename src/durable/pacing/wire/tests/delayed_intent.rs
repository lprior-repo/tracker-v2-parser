use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use tokio::time::Instant;

use crate::{
    durable::pacing::{
        PersistentPacing, WireRequest,
        test_support::{FixtureKind, WireFixture, with_database},
    },
    model::Source,
};

#[tokio::test]
async fn given_delayed_durable_intent_when_start_spaced_then_actual_operation_entries_stay_paced()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 200,
                body: "ok",
            })
            .await?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .build()?;
            let first_request = client.get(fixture.url.clone()).build()?;
            let second_request = client.get(fixture.url.clone()).build()?;
            let interval = Duration::from_millis(500);
            let first = PersistentPacing::new(pool.clone())
                .with_start_spacing()
                .with_wire_context(&"1".repeat(64), &"a".repeat(64))?;
            let second = PersistentPacing::new(pool.clone())
                .with_start_spacing()
                .with_wire_context(&"2".repeat(64), &"b".repeat(64))?;
            let identity = || {
                WireRequest::new(
                    Source::MileSplit,
                    "https://wi.milesplit.com/athletes/intent-wait-fixture",
                    "GET",
                    b"",
                )
            };
            let mut blocker = pool.begin().await?;
            sqlx::query("LOCK TABLE durable_wire_requests IN SHARE MODE")
                .execute(&mut *blocker)
                .await?;

            let sending = async {
                let mut entries = Vec::with_capacity(2);
                let response = first
                    .send(identity()?, interval, async {
                        entries.push(Instant::now());
                        client
                            .execute(first_request)
                            .await
                            .context("first owned HTTP request")
                    })
                    .await?;
                ensure!(response.status().as_u16() == 200, "first HTTP status");
                drop(response);
                let response = second
                    .send(identity()?, interval, async {
                        entries.push(Instant::now());
                        client
                            .execute(second_request)
                            .await
                            .context("second owned HTTP request")
                    })
                    .await?;
                ensure!(response.status().as_u16() == 200, "second HTTP status");
                Ok::<_, anyhow::Error>(entries)
            };
            let releasing = async {
                let leased = tokio::time::timeout(
                    Duration::from_secs(5),
                    stream::iter(0..200)
                        .map(Ok::<_, anyhow::Error>)
                        .try_fold(false, |observed, _| async move {
                            if observed {
                                return Ok(true);
                            }
                            let count = sqlx::query_scalar::<_, i64>(
                                "SELECT count(*) FROM durable_wire_pacing WHERE lease_token IS NOT NULL",
                            )
                            .fetch_one(pool)
                            .await?;
                            if count == 2 {
                                return Ok(true);
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            Ok(false)
                        }),
                )
                .await
                .context("observing committed pacing reservation")??;
                ensure!(leased, "first request never reserved its pacing scope");
                tokio::time::sleep(Duration::from_millis(750)).await;
                blocker.rollback().await?;
                Ok::<_, anyhow::Error>(())
            };
            let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                tokio::join!(sending, releasing)
            })
            .await;
            let arrivals = fixture.finish().await;
            let (sending, releasing) = outcome.context("owned pacing scenario timed out")?;
            releasing?;
            let entries = sending?;
            ensure!(arrivals?.len() == 2, "expected two real HTTP requests");
            let gap = entries
                .get(1)
                .context("second operation entry missing")?
                .duration_since(*entries.first().context("first operation entry missing")?);
            eprintln!(
                "legacy start spacing: interval=500ms; confirmed post-reservation SQL hold=750ms; actual operation entry gap={gap:?}"
            );
            assert!(
                gap >= Duration::from_millis(490),
                "post-reservation SQL delay consumed actual request-start spacing: {gap:?}"
            );
            Ok(())
        })
    })
    .await
}
