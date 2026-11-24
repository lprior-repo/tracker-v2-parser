use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::classify;
use crate::{
    database::operation_receipts::with_receipt, durable::pacing::test_support::with_database,
};

fn assert_classification(error: anyhow::Error, retryable: bool) {
    let classified = classify(error);
    let observable = format!("{classified:?}");
    let expected = if retryable {
        "HandlerError(Retryable("
    } else {
        "HandlerError(Terminal("
    };
    assert!(
        observable.starts_with(expected),
        "wrong Restate error classification: {observable}"
    );
}

#[test]
fn saturated_or_stopping_blocking_workers_are_retryable_through_recorded_effect_context() {
    use crate::durable::blocking::BlockingStageUnavailable;

    for state in [
        BlockingStageUnavailable::Busy,
        BlockingStageUnavailable::Stopping,
    ] {
        assert_classification(
            anyhow::Error::from(state)
                .context("delivering verified workbook")
                .context("recorded export effect"),
            true,
        );
    }
}

#[tokio::test(start_paused = true)]
async fn elapsed_runtime_timeout_is_retryable_through_persistent_wire_context() {
    let elapsed = tokio::time::timeout(std::time::Duration::ZERO, std::future::pending::<()>())
        .await
        .expect_err("pending fixture must reach its exact timeout");
    assert_classification(
        anyhow::Error::from(elapsed)
            .context("persistent wire admission exceeded bounded wait budget"),
        true,
    );
}

#[test]
fn sqlx_transport_loss_retries_but_invalid_socket_configuration_is_terminal() {
    use std::io::{Error, ErrorKind};

    for (kind, retryable) in [
        (ErrorKind::UnexpectedEof, true),
        (ErrorKind::ConnectionReset, true),
        (ErrorKind::BrokenPipe, true),
        (ErrorKind::InvalidInput, false),
        (ErrorKind::InvalidData, false),
        (ErrorKind::Unsupported, false),
        (ErrorKind::PermissionDenied, false),
    ] {
        assert_classification(
            anyhow::Error::from(sqlx::Error::Io(Error::from(kind)))
                .context("opening PostgreSQL connection for a recorded effect"),
            retryable,
        );
    }
}

#[test]
fn immutable_capacity_validation_and_access_stops_remain_terminal() {
    for message in [
        "frozen export exceeds capacity",
        "operation artifact exceeds streaming capacity",
        "durable effect receipt artifact mismatch",
        "invalid operation input",
        "owned blocking stage capacity of 8 is exhausted",
    ] {
        assert_classification(anyhow::anyhow!(message).context("recorded effect"), false);
    }
    assert_classification(
        anyhow::Error::from(crate::transport::AccessStop {
            origin: "https://wi.milesplit.com".into(),
            status: Some(403),
        })
        .context("download source access stop"),
        false,
    );
}

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

async fn failed_receipt_with_sqlstate(pool: &sqlx::PgPool, code: &str) -> Result<anyhow::Error> {
    let statement = format!(
        "DO $owned$ BEGIN RAISE SQLSTATE '{code}' USING MESSAGE='owned recovery fixture'; END $owned$;"
    );
    let failure: Result<Value> = with_receipt(
        pool,
        &format!("recovery-{code}"),
        &"a".repeat(64),
        move |connection| {
            Box::pin(async move {
                sqlx::query("INSERT INTO athletes(name) VALUES('Uncommitted recovery fixture')")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query(sqlx::AssertSqlSafe(statement))
                    .execute(connection)
                    .await?;
                Ok(json!({"unexpected":"committed"}))
            })
        },
    )
    .await;
    let failure = failure.expect_err("PostgreSQL must raise the requested SQLSTATE");
    let database_error = failure
        .chain()
        .find_map(|cause| cause.downcast_ref::<sqlx::Error>())
        .context("real SQLx database error absent")?;
    assert_eq!(
        database_error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some(code)
    );
    Ok(failure.context("journaled database effect"))
}

#[tokio::test]
async fn postgres_connection_contention_and_resource_failures_retry_without_receipts() -> Result<()>
{
    with_database(|pool| {
        Box::pin(async move {
            let before = committed_counts(pool).await?;
            for code in [
                "08001", "08003", "08006", "40001", "40P01", "55P03", "57P01", "57P02", "57P03",
                "57P05", "53300",
            ] {
                let failure = failed_receipt_with_sqlstate(pool, code).await?;
                assert_classification(failure, true);
                assert_eq!(committed_counts(pool).await?, before);
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn postgres_authentication_protocol_corruption_and_schema_failures_stay_terminal()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let before = committed_counts(pool).await?;
            for code in [
                "08004", "08P01", "28P01", "57P04", "23505", "42601", "54000", "XX001",
            ] {
                let failure = failed_receipt_with_sqlstate(pool, code).await?;
                assert_classification(failure, false);
                assert_eq!(committed_counts(pool).await?, before);
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn retrying_database_effect_after_recovery_commits_exactly_once() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let failure = failed_receipt_with_sqlstate(pool, "57P03").await?;
            assert_classification(failure, true);
            let first: Value = with_receipt(pool, "recovery-57P03", &"a".repeat(64), |connection| {
                Box::pin(async move {
                    sqlx::query("INSERT INTO athletes(name) VALUES('Recovered receipt fixture')")
                        .execute(connection)
                        .await?;
                    Ok(json!({"recovered":true}))
                })
            })
            .await?;
            let repeated: Value = with_receipt(pool, "recovery-57P03", &"a".repeat(64), |_| {
                Box::pin(async { anyhow::bail!("committed effect must not execute twice") })
            })
            .await?;
            assert_eq!(first, json!({"recovered":true}));
            assert_eq!(repeated, first);
            assert_eq!(
                committed_counts(pool).await?,
                json!({"athletes":1,"operation_receipts":1,"page_receipts":0,"pages":0,"wire_requests":0})
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn unknown_commit_outcome_retries_and_returns_the_already_committed_receipt() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let key = "ambiguous-commit";
            let digest = "a".repeat(64);
            let first: Result<Value> = async {
                let saved = with_receipt(pool, key, &digest, |connection| {
                    Box::pin(async move {
                        let id: i64 = sqlx::query_scalar(
                            "INSERT INTO athletes(name) VALUES('Ambiguous commit fixture') RETURNING id",
                        )
                        .fetch_one(connection)
                        .await?;
                        Ok(json!({"athlete_id":id}))
                    })
                })
                .await?;
                sqlx::query(
                    "DO $$ BEGIN RAISE SQLSTATE '08007' USING MESSAGE='owned lost-acknowledgement fixture'; END $$;",
                )
                .execute(pool)
                .await
                .context("observing operation commit acknowledgement")?;
                Ok(saved)
            }
            .await;
            let error = first.expect_err("PostgreSQL must report an unknown outcome");
            assert_eq!(
                error.downcast_ref::<sqlx::Error>()
                    .and_then(sqlx::Error::as_database_error)
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("08007")
            );
            assert_classification(error, true);
            let repeated: Value = with_receipt(pool, key, &digest, |_| {
                Box::pin(async { anyhow::bail!("ambiguous commit already applied") })
            })
            .await?;
            let id: i64 = sqlx::query_scalar("SELECT id FROM athletes")
                .fetch_one(pool)
                .await?;
            assert_eq!(repeated, json!({"athlete_id":id}));
            assert_eq!(
                committed_counts(pool).await?,
                json!({"athletes":1,"operation_receipts":1,"page_receipts":0,"pages":0,"wire_requests":0})
            );
            Ok(())
        })
    })
    .await
}
