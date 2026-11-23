use anyhow::{Context, Result};

use crate::durable::pacing::test_support::with_database;

#[tokio::test]
async fn given_generic_program_limit_when_classified_then_source_rejection_is_not_added()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let failure = sqlx::query(
                "DO $owned$ BEGIN RAISE SQLSTATE '54000' USING MESSAGE='index row requires 8216 bytes, maximum size is 8191'; END $owned$;",
            )
            .execute(pool)
            .await
            .expect_err("PostgreSQL must raise the requested program limit error");
            let database = failure
                .as_database_error()
                .and_then(|error| error.try_downcast_ref::<sqlx::postgres::PgDatabaseError>())
                .context("expected a structured PostgreSQL error")?;
            assert_eq!(database.code(), "54000");
            assert_eq!(database.routine(), Some("exec_stmt_raise"));
            let classified = super::classify(failure.into());
            assert!(!classified.is::<super::PageDataRejected>());
            Ok(())
        })
    })
    .await
}
