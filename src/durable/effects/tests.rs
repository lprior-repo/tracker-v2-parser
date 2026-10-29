use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::classify;
use crate::{
    database::operation_receipts::with_receipt, durable::pacing::test_support::with_database,
};

async fn committed_counts(pool: &sqlx::PgPool) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object( \
         'athletes',(SELECT count(*) FROM athletes), \
         'operation_receipts',(SELECT count(*) FROM durable_operation_receipts), \
         'page_receipts',(SELECT count(*) FROM durable_page_receipts), \
         'pages',(SELECT count(*) FROM scrape_pages), \
         'wire_requests',(SELECT count(*) FROM durable_wire_requests))",
    )
    .fetch_one(pool)
    .await?)
}

#[tokio::test]
async fn disk_full_sqlstate_retries_while_unique_violation_stays_terminal_without_committed_writes()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let before = committed_counts(pool).await?;
            assert_eq!(before, json!({"athletes":0,"operation_receipts":0,"page_receipts":0,"pages":0,"wire_requests":0}));
            let cases = [
                ("53100", "DO $owned$ BEGIN RAISE SQLSTATE '53100' USING MESSAGE='owned disk-full fixture'; END $owned$;", true),
                ("23505", "DO $owned$ BEGIN RAISE SQLSTATE '23505' USING MESSAGE='owned unique-violation fixture'; END $owned$;", false),
            ];
            for (code, statement, retryable) in cases {
                let failure: Result<Value> = with_receipt(
                    pool,
                    &format!("classification-{code}"),
                    &"a".repeat(64),
                    move |connection| Box::pin(async move {
                        sqlx::query("INSERT INTO athletes(name) VALUES('Uncommitted classification fixture')")
                            .execute(&mut *connection).await?;
                        sqlx::query(statement).execute(&mut *connection).await?;
                        Ok(json!({"unexpected":"committed"}))
                    }),
                ).await;
                let failure = failure.expect_err("PostgreSQL must raise the requested SQLSTATE");
                let database_error = failure.chain().find_map(|cause| cause.downcast_ref::<sqlx::Error>()).context("real SQLx database error absent")?;
                assert_eq!(database_error.as_database_error().and_then(|error|error.code()).as_deref(),Some(code));
                let classified = classify(failure.context("journaled queue SELECT fixture"));
                let observable = format!("{classified:?}");
                assert!(observable.starts_with(if retryable {"HandlerError(Retryable("} else {"HandlerError(Terminal("}), "wrong Restate error classification: {observable}");
                assert_eq!(committed_counts(pool).await?, before);
            }
            Ok(())
        })
    })
    .await
}
