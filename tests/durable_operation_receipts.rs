mod support;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use track_scrapper_v2::database::{Database, operation_receipts::with_receipt};

#[tokio::test]
async fn duplicate_operation_returns_committed_result_without_reapplying_domain_mutation()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let digest = "a".repeat(64);
            let first: Value = with_receipt(
                database.pool(),
                "reference-operation",
                &digest,
                |connection| {
                    Box::pin(async move {
                        let id: i64 = sqlx::query_scalar(
                            "INSERT INTO athletes(name) VALUES('Once') RETURNING id",
                        )
                        .fetch_one(connection)
                        .await?;
                        Ok(json!({"athlete_id":id}))
                    })
                },
            )
            .await?;
            let replay: Value =
                with_receipt(database.pool(), "reference-operation", &digest, |_| {
                    Box::pin(async { Err(anyhow!("duplicate operation must not execute")) })
                })
                .await?;
            assert_eq!(replay, first);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM athletes WHERE name='Once'")
                    .fetch_one(database.pool())
                    .await?,
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(database.pool())
                    .await?,
                1
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn reused_operation_key_with_changed_digest_rejects_before_mutating_domain() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let _: Value = with_receipt(database.pool(),"same-operation",&"a".repeat(64),|_| Box::pin(async {Ok(json!({"version":1}))})).await?;
        let changed: Result<Value> = with_receipt(database.pool(),"same-operation",&"b".repeat(64),|connection| Box::pin(async move {
            sqlx::query("INSERT INTO athletes(name) VALUES('Forbidden')").execute(connection).await?;
            Ok(json!({"version":2}))
        })).await;
        assert!(changed.err().expect("mismatched operation must fail").to_string().contains("different input"));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM athletes").fetch_one(database.pool()).await?,0);
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT input_sha256 FROM durable_operation_receipts WHERE operation_key='same-operation'").fetch_one(database.pool()).await?,"a".repeat(64));
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn failed_operation_rolls_back_domain_and_receipt_then_retry_commits_together() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let digest = "c".repeat(64);
            let failed: Result<Value> =
                with_receipt(database.pool(), "retry-operation", &digest, |connection| {
                    Box::pin(async move {
                        sqlx::query("INSERT INTO athletes(name) VALUES('Partial')")
                            .execute(connection)
                            .await?;
                        Err(anyhow!("injected transaction failure"))
                    })
                })
                .await;
            assert!(
                failed
                    .err()
                    .expect("fault must propagate")
                    .to_string()
                    .contains("injected transaction failure")
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM athletes")
                    .fetch_one(database.pool())
                    .await?,
                0
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(database.pool())
                    .await?,
                0
            );
            let retried: Value =
                with_receipt(database.pool(), "retry-operation", &digest, |connection| {
                    Box::pin(async move {
                        sqlx::query("INSERT INTO athletes(name) VALUES('Complete')")
                            .execute(connection)
                            .await?;
                        Ok(json!({"status":"committed"}))
                    })
                })
                .await?;
            assert_eq!(retried, json!({"status":"committed"}));
            assert_eq!(
                sqlx::query_scalar::<_, String>("SELECT name FROM athletes")
                    .fetch_one(database.pool())
                    .await?,
                "Complete"
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(database.pool())
                    .await?,
                1
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
