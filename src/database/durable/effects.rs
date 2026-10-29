use anyhow::{Context, Result, ensure};

use super::{Database, lock_reservation};
use crate::{
    durable::page::{CaptureRef, PageOutcome, PageReservation, ParsedRef},
    model::{Capture, ParsedPage},
};

impl Database {
    pub async fn durable_enqueue_receipt(
        &self,
        key: &str,
        digest: &str,
        targets: &[crate::model::Target],
        report: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let targets = targets.to_vec();
        super::super::operation_receipts::with_receipt(&self.pool, key, digest, move |connection| {
            Box::pin(async move {
                let enqueued = super::super::queue::enqueue(connection, &targets, false).await?;
                let mut report = report;
                report
                    .as_object_mut()
                    .context("WIAA report must be an object")?
                    .insert("enqueued".to_owned(), serde_json::json!(enqueued));
                Ok(report)
            })
        })
        .await
    }

    pub async fn durable_persist(
        &self,
        reservation: &PageReservation,
        capture_ref: &CaptureRef,
        parsed_ref: &ParsedRef,
        capture: &Capture,
        parsed: &ParsedPage,
    ) -> Result<PageOutcome> {
        let mut transaction = self.pool.begin().await?;
        lock_reservation(&mut transaction, reservation).await?;
        let capture_json = serde_json::to_value(capture_ref)?;
        let parsed_json = serde_json::to_value(parsed_ref)?;
        let existing: Option<(serde_json::Value, serde_json::Value, serde_json::Value)> =
            sqlx::query_as(
                "SELECT capture,parsed,outcome FROM durable_page_receipts WHERE operation_id=$1",
            )
            .bind(&reservation.request.binding.operation_id)
            .fetch_optional(&mut *transaction)
            .await?;
        if let Some((saved_capture, saved_parsed, outcome)) = existing {
            ensure!(
                saved_capture == capture_json && saved_parsed == parsed_json,
                "durable effect receipt artifact mismatch"
            );
            let outcome = serde_json::from_value(outcome)?;
            transaction.commit().await?;
            return Ok(outcome);
        }
        let (saved_capture, saved_parsed, state): (
            Option<serde_json::Value>,
            Option<serde_json::Value>,
            String,
        ) = sqlx::query_as(
            "SELECT capture,parsed,state FROM durable_page_attempts WHERE attempt_id=$1",
        )
        .bind(&reservation.attempt_id)
        .fetch_one(&mut *transaction)
        .await?;
        ensure!(
            saved_capture.as_ref() == Some(&capture_json)
                && saved_parsed.as_ref() == Some(&parsed_json)
                && state == "parsed",
            "durable persistence requires its exact recorded parse and capture"
        );
        ensure!(
            capture.url == capture_ref.url
                && capture.status == capture_ref.status
                && capture.content_type == capture_ref.content_type
                && crate::durable::page::body_digest(&capture.body) == capture_ref.body.sha256,
            "durable effect capture bytes mismatch"
        );
        ensure!(
            crate::durable::page::value_digest(parsed)? == parsed_ref.body.sha256,
            "durable effect parsed bytes mismatch"
        );
        let request = &reservation.request;
        let page_id = super::super::records::persist(
            &mut transaction,
            Some(request.binding.page_id),
            &request.target,
            capture,
            parsed,
        )
        .await?;
        let receipt_sha256 = crate::durable::page::value_digest(&(
            &request.binding,
            &reservation.attempt_id,
            capture_ref,
            parsed_ref,
        ))?;
        let outcome = PageOutcome::Completed {
            page_id,
            receipt_sha256: receipt_sha256.clone(),
        };
        let value = serde_json::to_value(&outcome)?;
        sqlx::query("INSERT INTO durable_page_receipts(operation_id,attempt_id,page_id,capture,parsed,receipt_sha256,outcome) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(&request.binding.operation_id).bind(&reservation.attempt_id).bind(page_id).bind(capture_json).bind(parsed_json).bind(receipt_sha256).bind(&value).execute(&mut *transaction).await?;
        sqlx::query(
            "UPDATE durable_page_attempts SET state='completed',outcome=$2 WHERE attempt_id=$1",
        )
        .bind(&reservation.attempt_id)
        .bind(&value)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE durable_page_operations SET state='completed',retry_at_ms=NULL,outcome=$2 WHERE operation_id=$1")
            .bind(&request.binding.operation_id).bind(value).execute(&mut *transaction).await?;
        transaction
            .commit()
            .await
            .context("committing durable domain effects and receipt")?;
        Ok(outcome)
    }
}
