#[path = "review/support.rs"]
mod review_support;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::database::Database;

async fn bounded_ledger_fixture(database: &Database) -> Result<i64> {
    let mut first = 0;
    for (index, state) in ["WI", "MI", "WI"].into_iter().enumerate() {
        let target = review_support::athlete(&(index + 100).to_string(), Some(state))?;
        let id = review_support::failure(database, &target, "failed", 1, state).await?;
        if index == 0 {
            first = id;
        }
        sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts,retry_at_ms) VALUES(lpad($1::text,64,'0'),$2,'bounded-review','{}','deferred',1,1793278800000)")
            .bind(index.to_string())
            .bind(id)
            .execute(database.pool())
            .await?;
    }
    sqlx::query("ALTER TABLE durable_page_operations RENAME TO review_ledger_storage")
        .execute(database.pool())
        .await?;
    sqlx::query("CREATE FUNCTION review_ledger_state(value text,page bigint,allowed bigint) RETURNS text LANGUAGE plpgsql VOLATILE AS $body$ BEGIN IF page<>allowed THEN RAISE EXCEPTION 'review evaluated an excluded or unpaged ledger row'; END IF; RETURN value; END $body$")
        .execute(database.pool())
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE VIEW durable_page_operations AS SELECT operation_id,page_id,run_generation,binding,review_ledger_state(state,page_id,{first}) AS state,cycle,cycle_attempts,original_attempts,retry_at_ms,outcome,created_at FROM review_ledger_storage"
    )))
    .execute(database.pool())
    .await?;
    Ok(first)
}

#[tokio::test]
async fn given_other_cohorts_and_later_pages_when_reviewing_one_row_then_only_its_ledger_is_read()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let first = bounded_ledger_fixture(&database).await?;
            let mut snapshot = database
                .begin_export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            let rows = snapshot.review_records(0, 1).await?;
            assert_eq!(rows.len(), 1);
            let row = rows.first().context("bounded review row")?;
            assert_eq!(row.checkpoint_id, first);
            assert_eq!(row.state, "WI");
            assert_eq!(row.durable_state, "deferred");
            assert_eq!(row.error, "WI");
            assert!(!row.retry_after.is_empty());
            snapshot.finish().await?;
            let guard_error = sqlx::query_scalar::<_, String>(
                "SELECT state FROM durable_page_operations WHERE page_id<>$1 LIMIT 1",
            )
            .bind(first)
            .fetch_one(database.pool())
            .await
            .err()
            .context("excluded ledger must trigger the test guard")?;
            assert!(
                guard_error
                    .to_string()
                    .contains("excluded or unpaged ledger row")
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
