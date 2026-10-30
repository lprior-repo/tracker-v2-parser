use std::time::Duration;

use anyhow::{Context, Result, ensure};
use sqlx::{PgPool, postgres::PgPoolOptions};

use super::super::{PersistentPacing, Spacing, WireScope, test_support::with_database};
use crate::model::Source;

#[tokio::test]
async fn completion_waiting_for_origin_does_not_lock_provider_out_of_order() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let scope = WireScope::parse(Source::MileSplit, "https://school.example.edu/", Duration::from_millis(10), Spacing::CompletedHeaders)?;
        let token = "a".repeat(32);
        sqlx::query("INSERT INTO durable_wire_pacing(pacing_key,lease_token) VALUES ($1,$3),($2,$3)")
            .bind(&scope.keys[0]).bind(&scope.keys[1]).bind(&token).execute(pool).await?;
        let completion_pool = PgPoolOptions::new().max_connections(1).after_connect(|connection, _| Box::pin(async move {
            sqlx::raw_sql("SET enable_indexscan=off; SET enable_bitmapscan=off").execute(connection).await?;
            Ok(())
        })).connect_with(pool.connect_options().as_ref().clone()).await?;
        let completion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&completion_pool).await?;
        let mut blocker = pool.begin().await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *blocker).await?;
        sqlx::query("SELECT pacing_key FROM durable_wire_pacing WHERE pacing_key=$1 FOR UPDATE")
            .bind(&scope.keys[1]).fetch_one(&mut *blocker).await?;
        let gate = PersistentPacing::new(completion_pool.clone());
        let (completed, observed) = tokio::join!(tokio::time::timeout(Duration::from_secs(6), gate.complete(&scope, &token)), async {
            let observed = wait_for_blocker(pool, completion_pid, blocker_pid).await;
            let probe = match observed {
                Ok(()) => sqlx::query("SELECT pacing_key FROM durable_wire_pacing WHERE pacing_key=$1 FOR UPDATE NOWAIT")
                    .bind(&scope.keys[0]).fetch_one(pool).await.map(|_| ()).map_err(anyhow::Error::from),
                Err(error) => Err(error),
            };
            blocker.rollback().await?;
            probe
        });
        completion_pool.close().await;
        completed.context("completion did not drain after releasing its blocker")??;
        observed.context("completion acquired provider before its blocked origin")?;
        let released: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_wire_pacing WHERE lease_token IS NULL")
            .fetch_one(pool).await?;
        assert_eq!(released, 2);
        Ok(())
    })).await
}

async fn wait_for_blocker(pool: &PgPool, waiting: i32, blocking: i32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        for _ in 0..300 {
            let blocked: bool = sqlx::query_scalar("SELECT $2=ANY(pg_blocking_pids($1))")
                .bind(waiting)
                .bind(blocking)
                .fetch_one(pool)
                .await?;
            if blocked {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        ensure!(false, "completion never waited on the held origin");
        Ok(())
    })
    .await
    .context("waiting for PostgreSQL's observed lock dependency")?
}

#[tokio::test]
async fn mismatched_completion_token_rolls_back_every_pacing_row() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let scope = WireScope::parse(
                Source::MileSplit,
                "https://school.example.edu/",
                Duration::from_millis(10),
                Spacing::CompletedHeaders,
            )?;
            let token = "a".repeat(32);
            sqlx::query(
                "INSERT INTO durable_wire_pacing(pacing_key,lease_token) VALUES ($1,$3),($2,$4)",
            )
            .bind(&scope.keys[0])
            .bind(&scope.keys[1])
            .bind(&token)
            .bind("b".repeat(32))
            .execute(pool)
            .await?;
            let before: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY pacing_key) FROM durable_wire_pacing p",
            )
            .fetch_one(pool)
            .await?;
            let error = PersistentPacing::new(pool.clone())
                .complete(&scope, &token)
                .await
                .expect_err("mismatched lease must fail");
            assert!(
                error
                    .to_string()
                    .contains("lease changed before completion")
            );
            let after: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY pacing_key) FROM durable_wire_pacing p",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(after, before);
            Ok(())
        })
    })
    .await
}
