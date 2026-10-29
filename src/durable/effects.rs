use restate_sdk::prelude::*;

macro_rules! recorded {
    ($ctx:expr, $name:expr, $effect:expr) => {{
        let effect = $effect;
        let result = $ctx
            .run(move || async move {
                effect()
                    .await
                    .map(restate_sdk::prelude::Json)
                    .map_err($crate::durable::effects::classify)
            })
            .name($name)
            .retry_policy(
                restate_sdk::prelude::RunRetryPolicy::new()
                    .initial_delay(std::time::Duration::from_secs(1))
                    .exponentiation_factor(2.0)
                    .max_delay(std::time::Duration::from_mins(5))
                    .max_attempts(10_000),
            )
            .await;
        result
            .map(|restate_sdk::prelude::Json(value)| value)
            .map_err(restate_sdk::prelude::HandlerError::from)
    }};
}

macro_rules! current_time {
    ($ctx:expr) => {
        $crate::durable::effects::record!($ctx, "record-current-time", || async {
            Ok(chrono::Utc::now().timestamp_millis())
        })
    };
}

pub(super) use current_time as now;
pub(super) use recorded as record;

pub(super) fn classify(error: anyhow::Error) -> HandlerError {
    let transient = error.chain().any(|cause| {
        if cause
            .downcast_ref::<crate::school_address_download::DownloadTransient>()
            .is_some()
        {
            return true;
        }
        if cause
            .downcast_ref::<sqlx::Error>()
            .is_some_and(|error| match error {
                sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
                sqlx::Error::Database(database) => database.code().is_some_and(|code| {
                    matches!(
                        code.as_ref(),
                        "40001" | "40P01" | "55P03" | "57P01" | "53300"
                    )
                }),
                _ => false,
            })
        {
            return true;
        }
        if cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|error| error.is_connect() || error.is_timeout() || error.is_body())
        {
            return true;
        }
        if cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::StorageFull
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
            )
        }) {
            return true;
        }
        cause
            .downcast_ref::<crate::durable::AcquisitionBusy>()
            .is_some()
    });
    if transient {
        HandlerError::from(error)
    } else {
        TerminalError::new(format!("{error:#}")).into()
    }
}

pub(super) async fn acquisition(
    database: &crate::database::Database,
) -> anyhow::Result<sqlx::Transaction<'_, sqlx::Postgres>> {
    let mut transaction = database.pool().begin().await?;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtext(current_schema()),194832741)",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if acquired {
        Ok(transaction)
    } else {
        transaction.rollback().await?;
        Err(crate::durable::AcquisitionBusy.into())
    }
}
