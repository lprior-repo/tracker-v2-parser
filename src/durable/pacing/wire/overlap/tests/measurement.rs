use super::*;

#[tokio::test]
async fn nullable_execute_measurement_migration_preserves_historical_wire_rows() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        sqlx::raw_sql("DROP INDEX durable_wire_requests_pending_source_started_idx; ALTER TABLE durable_wire_requests DROP COLUMN execute_started_at")
            .execute(pool).await?;
        let gate = PersistentPacing::new(pool.clone()).with_wire_context(&"1".repeat(64), &"b".repeat(64))?;
        let request = request()?;
        let receipt = gate.wire_context.as_ref().context("context")?.next(&request)?;
        super::super::super::store::insert_wire(pool, &receipt, &request).await?;
        let before: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(w) FROM durable_wire_requests w").fetch_one(pool).await?;
        sqlx::raw_sql(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/migrations/202611010003_pending_wire_admission_lookup.sql")))
            .execute(pool).await?;
        let after: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(w)-'execute_started_at' FROM durable_wire_requests w").fetch_one(pool).await?;
        assert_eq!(before, after);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests WHERE execute_started_at IS NULL AND state='intent'").fetch_one(pool).await?,1);
        Ok(())
    })).await
}

#[tokio::test]
async fn cancellation_while_waiting_for_local_fence_retains_unconfirmed_intent() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let gate = PersistentPacing::new(pool.clone()).with_overlapped_starts();
        let hold = gate.starts.admit(Source::MileSplit,Duration::from_millis(1)).await?;
        let sending = gate.clone().with_wire_context(&"1".repeat(64), &"b".repeat(64))?;
        let mut called = false;
        assert!(tokio::time::timeout(Duration::from_millis(100),sending.send(request()?,Duration::from_millis(1),async {
            called = true;
            anyhow::bail!("blocked fence must remain prewire")
        })).await.is_err());
        assert!(!called);
        drop(hold);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests WHERE state='intent' AND execute_started_at IS NULL").fetch_one(pool).await?,1);
        let replay = sending.restart_wire_sequence()?;
        assert!(replay.send(request()?,Duration::from_millis(1),async {
            called = true;
            anyhow::bail!("same cancelled attempt cannot be reacquired")
        }).await.is_err());
        assert!(!called);
        Ok(())
    })).await
}

#[tokio::test]
async fn local_start_fences_keep_providers_independent() -> Result<()> {
    let starts = crate::durable::pacing::start::StartFences::default();
    let first = starts
        .admit(Source::MileSplit, Duration::from_millis(1))
        .await?;
    let second = tokio::time::timeout(
        Duration::from_millis(100),
        starts.admit(Source::AthleticNet, Duration::from_millis(1)),
    )
    .await??;
    drop((first, second));
    Ok(())
}

#[tokio::test]
async fn overlapping_waiter_rechecks_new_403_before_client_execute() -> Result<()> {
    use crate::durable::pacing::test_support::{FixtureKind, WireFixture};
    with_database(|pool| {
        Box::pin(async move {
            let fixture = WireFixture::start(FixtureKind::DelayedPage {
                status: 403,
                body: "denied",
                delay: Duration::from_millis(350),
            })
            .await?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .build()?;
            let gate = PersistentPacing::new(pool.clone()).with_overlapped_starts();
            let results = join_all((1..=2).map(|index| {
                let client = &client;
                let url = fixture.url.clone();
                let gate = gate.clone();
                async move {
                    gate
                        .with_wire_context(&format!("{index:064x}"), &"b".repeat(64))?
                        .send(request()?, Duration::from_millis(500), async {
                            Ok(client.get(url).send().await?)
                        })
                        .await
                }
            }))
            .await;
            assert_eq!(
                results
                    .iter()
                    .filter(|result| result
                        .as_ref()
                        .is_ok_and(|response| response.status().as_u16() == 403))
                    .count(),
                1
            );
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests WHERE execute_started_at IS NOT NULL AND status=403").fetch_one(pool).await?,1);
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests WHERE execute_started_at IS NULL AND state='failed'").fetch_one(pool).await?,1);
            assert_eq!(
                results
                    .iter()
                    .filter(|result| result
                        .as_ref()
                        .is_err_and(|error| error.is::<crate::transport::AccessStop>()))
                    .count(),
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                2
            );
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}
