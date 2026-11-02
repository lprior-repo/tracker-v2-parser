mod effects;
mod exclusion;
mod lineage;
mod reservation;
mod selection;
mod stages;

use anyhow::{Context, Result, ensure};
use sqlx::PgConnection;

use crate::durable::{
    model::DeadlineExtensionAuthorization,
    page::{PageDeadlineExtension, PageOutcome, PageRequest, PageReservation, PageScope},
};

use super::Database;

impl Database {
    pub(crate) async fn durable_request_with_deadline_extension(
        &self,
        job: crate::database::Job,
        scope: &PageScope,
        authorization: &DeadlineExtensionAuthorization,
    ) -> Result<PageRequest> {
        ensure!(
            authorization.effective_deadline_ms == scope.deadline_ms,
            "deadline extension authorization does not match selected scope"
        );
        ensure!(
            authorization.previous_deadline_ms < authorization.effective_deadline_ms,
            "deadline extension authorization must increase the prior deadline"
        );
        let existing: Option<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT operation_id,binding FROM durable_page_operations WHERE page_id=$1 AND run_generation=$2",
        )
        .bind(job.id)
        .bind(&scope.run_generation)
        .fetch_optional(&self.pool)
        .await?;
        let Some((operation_id, binding_value)) = existing else {
            return crate::durable::page::request(job, scope);
        };
        let binding: crate::durable::page::PageBinding =
            serde_json::from_value(binding_value).context("decoding retained extension owner")?;
        let target_sha256 = crate::durable::page::value_digest(&job.target)?;
        ensure!(
            binding.operation_id == operation_id
                && binding.page_id == job.id
                && binding.target_sha256 == target_sha256,
            "retained deadline extension owner does not match selected page"
        );
        let old_scope = &binding.scope;
        ensure!(
            old_scope.run_generation == scope.run_generation
                && old_scope.config_sha256 == scope.config_sha256
                && old_scope.cooldown_ms == scope.cooldown_ms
                && old_scope.inherited_retry_campaigns == scope.inherited_retry_campaigns,
            "retained operation is not compatible with the explicitly authorized deadline extension"
        );
        if old_scope.deadline_ms == scope.deadline_ms {
            let request = crate::durable::page::request(job, scope)?;
            ensure!(
                request.binding == binding,
                "retained operation identity does not match its canonical current scope"
            );
            return Ok(request);
        }
        ensure!(
            old_scope.deadline_ms == authorization.previous_deadline_ms,
            "retained operation is not compatible with the explicitly authorized deadline extension"
        );
        let request = PageRequest {
            binding,
            target: job.target,
            deadline_extension: Some(PageDeadlineExtension {
                owner_operation_id: operation_id,
                previous_deadline_ms: authorization.previous_deadline_ms,
                effective_deadline_ms: authorization.effective_deadline_ms,
            }),
        };
        crate::durable::page::validate_request(&request)?;
        Ok(request)
    }

    #[cfg(test)]
    pub(crate) fn durable_test_database(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            coverage: super::coverage_scheduler::Cache::default(),
        }
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
