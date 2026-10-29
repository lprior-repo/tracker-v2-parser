mod effects;
mod reservation;
mod selection;
mod stages;

use anyhow::{Context, Result, ensure};
use sqlx::PgConnection;

use crate::durable::page::{PageOutcome, PageRequest, PageReservation};

use super::Database;

impl Database {
    #[cfg(test)]
    pub(crate) fn durable_test_database(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
    pub async fn durable_outcome(&self, request: &PageRequest) -> Result<Option<PageOutcome>> {
        crate::durable::page::validate_request(request)?;
        let row: Option<(serde_json::Value, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT binding,outcome FROM durable_page_operations WHERE operation_id=$1",
        )
        .bind(&request.binding.operation_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|(binding, outcome)| {
            ensure!(
                binding == serde_json::to_value(&request.binding)?,
                "durable operation binding mismatch"
            );
            outcome
                .map(serde_json::from_value)
                .transpose()
                .context("decoding durable page outcome")
        })
        .transpose()
        .map(Option::flatten)
    }

    pub async fn durable_due(&self, generation: &str) -> Result<Option<i64>> {
        sqlx::query_scalar("SELECT min(retry_at_ms) FROM durable_page_operations WHERE run_generation=$1 AND state='deferred'")
            .bind(generation).fetch_one(&self.pool).await.context("reading deferred page wakeup")
    }
}

async fn lock_request(
    connection: &mut PgConnection,
    request: &PageRequest,
) -> Result<(String, i32, i32, Option<i64>, Option<serde_json::Value>)> {
    crate::durable::page::validate_request(request)?;
    let (cursor, status): (serde_json::Value, String) =
        sqlx::query_as("SELECT cursor,status FROM scrape_pages WHERE id=$1 FOR UPDATE")
            .bind(request.binding.page_id)
            .fetch_one(&mut *connection)
            .await?;
    let (binding,state,cycle,attempts,due,outcome): (serde_json::Value,String,i32,i32,Option<i64>,Option<serde_json::Value>) = sqlx::query_as("SELECT binding,state,cycle,cycle_attempts,retry_at_ms,outcome FROM durable_page_operations WHERE operation_id=$1 FOR UPDATE")
        .bind(&request.binding.operation_id).fetch_one(&mut *connection).await?;
    ensure!(
        binding == serde_json::to_value(&request.binding)?,
        "durable operation binding mismatch"
    );
    let target: crate::model::Target = serde_json::from_value(cursor)?;
    ensure!(
        serde_json::to_value(target)? == serde_json::to_value(&request.target)?,
        "checkpoint target changed after durable binding"
    );
    ensure!(
        status != "blocked" || state == "blocked",
        "checkpoint acquired an access stop"
    );
    Ok((state, cycle, attempts, due, outcome))
}

async fn lock_reservation(
    connection: &mut PgConnection,
    reservation: &PageReservation,
) -> Result<()> {
    lock_request(connection, &reservation.request).await?;
    let actual: (String,i32,i32,i32) = sqlx::query_as("SELECT operation_id,cycle,ordinal,total_attempts FROM durable_page_attempts WHERE attempt_id=$1 FOR UPDATE")
        .bind(&reservation.attempt_id).fetch_one(connection).await?;
    ensure!(
        actual
            == (
                reservation.request.binding.operation_id.clone(),
                reservation.cycle,
                reservation.ordinal,
                reservation.total_attempts
            ),
        "durable reservation binding mismatch"
    );
    Ok(())
}
