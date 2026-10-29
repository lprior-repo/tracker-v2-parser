use anyhow::{Result, ensure};
use sqlx::{Postgres, Transaction};

use crate::database::Database;

#[tracing::instrument(skip(database))]
pub(super) async fn acquire(
    database: &Database,
    concurrency: usize,
    browser: bool,
) -> Result<Transaction<'_, Postgres>> {
    ensure!(
        (1..=3).contains(&concurrency),
        "invalid acquisition concurrency"
    );
    if concurrency == 1 {
        return super::effects::acquisition(database).await;
    }
    let mut transaction = database.pool().begin().await?;
    let shared: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock_shared(hashtext(current_schema()),194832741)",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if !shared {
        transaction.rollback().await?;
        return Err(super::AcquisitionBusy.into());
    }
    let slot: Option<i32> = sqlx::query_scalar(
        "SELECT slot FROM generate_series(0,$1::integer-1) slot \
         WHERE pg_try_advisory_xact_lock(hashtext(current_schema()),194832750+slot) LIMIT 1",
    )
    .bind(i32::try_from(concurrency)?)
    .fetch_optional(&mut *transaction)
    .await?;
    if slot.is_none() {
        transaction.rollback().await?;
        return Err(super::AcquisitionBusy.into());
    }
    if browser {
        let acquired: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtext(current_schema()),194832749)",
        )
        .fetch_one(&mut *transaction)
        .await?;
        if !acquired {
            transaction.rollback().await?;
            return Err(super::AcquisitionBusy.into());
        }
    }
    Ok(transaction)
}

#[cfg(test)]
mod tests;
