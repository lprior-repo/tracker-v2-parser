mod support;

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use anyhow::{Context, Result};
use futures::future::join_all;
use sqlx::Row;
use track_scrapper_v2::{database::Database, durable::pacing::PersistentPacing, model::Source};

#[tokio::test]
async fn given_fresh_pacing_instances_when_requests_cross_providers_or_origins_then_both_limits_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let interval = Duration::from_millis(500);
            let first = PersistentPacing::new(test.pool.clone());
            first
                .execute(
                    Source::MileSplit,
                    "https://wi.milesplit.com/one",
                    interval,
                    async { Ok(()) },
                )
                .await?;
            drop(first);
            let start = tokio::time::Instant::now();
            PersistentPacing::new(test.pool.clone())
                .execute(
                    Source::MileSplit,
                    "https://mn.milesplit.com/two",
                    interval,
                    async { Ok(()) },
                )
                .await?;
            assert!(start.elapsed() >= Duration::from_millis(450));
            let start = tokio::time::Instant::now();
            PersistentPacing::new(test.pool.clone())
                .execute(
                    Source::AthleticNet,
                    "https://mn.milesplit.com/three",
                    interval,
                    async { Ok(()) },
                )
                .await?;
            assert!(start.elapsed() >= Duration::from_millis(450));
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM durable_wire_pacing")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 4);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_concurrent_fresh_gates_when_they_share_a_provider_then_they_cannot_reserve_a_burst()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let arrivals = std::sync::Mutex::new(Vec::new());
            let result = join_all((0..3).map(|_| {
                let gate = PersistentPacing::new(test.pool.clone());
                let arrivals = &arrivals;
                async move {
                    gate.execute(
                        Source::MileSplit,
                        "https://wi.milesplit.com/athlete",
                        Duration::from_millis(500),
                        async {
                            arrivals
                                .lock()
                                .expect("arrival state")
                                .push(tokio::time::Instant::now());
                            Ok(())
                        },
                    )
                    .await
                }
            }))
            .await;
            for item in result {
                item?;
            }
            let times = arrivals.into_inner().expect("arrival state");
            assert_eq!(times.len(), 3);
            for pair in times.windows(2) {
                assert!(pair[1].duration_since(pair[0]) >= Duration::from_millis(450));
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_cancelled_wire_effect_when_a_new_gate_starts_then_committed_reservation_blocks_it()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let calls = AtomicUsize::new(0);
        let gate = PersistentPacing::new(test.pool.clone());
        let (entered, received) = tokio::sync::oneshot::channel();
        {
            let operation = gate.execute(Source::MileSplit, "https://wi.milesplit.com/a", Duration::from_millis(500), async {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.send(()).expect("effect observer");
                futures::future::pending::<Result<()>>().await
            });
            tokio::pin!(operation);
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    entered = received => entered.context("effect observer closed"),
                    result = &mut operation => anyhow::bail!("effect completed before cancellation: {result:?}"),
                }
            }).await??;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let row = sqlx::query("SELECT COUNT(*) AS count, MIN(EXTRACT(EPOCH FROM (next_allowed_at-clock_timestamp())))::double precision AS seconds FROM durable_wire_pacing WHERE lease_token IS NOT NULL")
            .fetch_one(&test.pool).await?;
        assert_eq!(row.try_get::<i64, _>("count")?, 2);
        assert!(row.try_get::<f64, _>("seconds")? > 40.0);
        drop(gate);
        let next = PersistentPacing::new(test.pool.clone());
        assert!(tokio::time::timeout(Duration::from_millis(75), next.execute(Source::MileSplit, "https://wi.milesplit.com/b", Duration::from_millis(500), async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_slow_contacts_when_completed_then_cooldown_persists_without_a_long_test_sleep()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        for (index, interval) in [(0, 5_u64), (1, 30)] {
            let source = if index == 0 { Source::MileSplit } else { Source::AthleticNet };
            let url = format!("https://school-{index}.example.edu/staff-directory");
            PersistentPacing::new(test.pool.clone()).execute(source, &url, Duration::from_secs(interval), async { Ok(()) }).await?;
            let seconds: i64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM (next_allowed_at-updated_at))::bigint FROM durable_wire_pacing WHERE pacing_key=$1")
                .bind(format!("origin:https://school-{index}.example.edu")).fetch_one(&test.pool).await?;
            assert_eq!(seconds, i64::try_from(interval)?);
        }
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_invalid_pacing_origins_when_admitted_then_no_effect_or_rows_are_created()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let calls = AtomicUsize::new(0);
            let gate = PersistentPacing::new(test.pool.clone());
            for address in [
                "file:///private",
                "https://user:secret@example.edu/",
                "invalid",
            ] {
                assert!(
                    gate.execute(
                        Source::MileSplit,
                        address,
                        Duration::from_millis(500),
                        async {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }
                    )
                    .await
                    .is_err()
                );
            }
            assert!(
                gate.execute(
                    Source::MileSplit,
                    "https://example.edu/",
                    Duration::ZERO,
                    async { Ok(()) }
                )
                .await
                .is_err()
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM durable_wire_pacing")
                .fetch_one(&test.pool)
                .await
                .context("reading pace rows")?;
            assert_eq!(count, 0);
            database.close().await;
            Ok(())
        })
    })
    .await
}
