use anyhow::{Context, Result, ensure};

use super::{Database, lock_reservation};
use crate::{
    database::Job,
    durable::page::{
        CaptureRef, FailureClass, FailureRecord, PageOutcome, PageReservation, ParsedRef,
        RetryPolicy,
    },
};

pub type SavedStages = (Option<CaptureRef>, Option<ParsedRef>, Option<PageOutcome>);

impl Database {
    pub async fn durable_stages(&self, reservation: &PageReservation) -> Result<SavedStages> {
        let mut transaction = self.pool.begin().await?;
        lock_reservation(&mut transaction, reservation).await?;
        let (capture, parsed, outcome): (
            Option<serde_json::Value>,
            Option<serde_json::Value>,
            Option<serde_json::Value>,
        ) = sqlx::query_as(
            "SELECT capture,parsed,outcome FROM durable_page_attempts WHERE attempt_id=$1",
        )
        .bind(&reservation.attempt_id)
        .fetch_one(&mut *transaction)
        .await?;
        let result = Ok((
            capture.map(serde_json::from_value).transpose()?,
            parsed.map(serde_json::from_value).transpose()?,
            outcome.map(serde_json::from_value).transpose()?,
        ));
        super::super::finish(transaction, result).await
    }

    pub async fn durable_record_capture(
        &self,
        reservation: &PageReservation,
        capture: &CaptureRef,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        lock_reservation(&mut transaction, reservation).await?;
        let value = serde_json::to_value(capture)?;
        let existing: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT capture FROM durable_page_attempts WHERE attempt_id=$1")
                .bind(&reservation.attempt_id)
                .fetch_one(&mut *transaction)
                .await?;
        ensure!(
            existing.as_ref().is_none_or(|existing| existing == &value),
            "durable captured response differs from retained stage"
        );
        let changed = sqlx::query("UPDATE durable_page_attempts SET capture=$2,state=CASE WHEN state='reserved' THEN 'captured' ELSE state END WHERE attempt_id=$1 AND state IN ('reserved','captured','parsed','completed')")
            .bind(&reservation.attempt_id).bind(value).execute(&mut *transaction).await?.rows_affected();
        ensure!(changed == 1, "capture cannot replace a failed attempt");
        transaction
            .commit()
            .await
            .context("committing durable captured stage")
    }

    pub async fn durable_record_parse(
        &self,
        reservation: &PageReservation,
        parsed: &ParsedRef,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        lock_reservation(&mut transaction, reservation).await?;
        let (capture, existing): (Option<serde_json::Value>, Option<serde_json::Value>) =
            sqlx::query_as("SELECT capture,parsed FROM durable_page_attempts WHERE attempt_id=$1")
                .bind(&reservation.attempt_id)
                .fetch_one(&mut *transaction)
                .await?;
        let capture: CaptureRef =
            serde_json::from_value(capture.context("parse requires a retained capture")?)?;
        ensure!(
            crate::durable::page::capture_digest(&capture)? == parsed.capture_sha256,
            "parsed stage capture binding mismatch"
        );
        let value = serde_json::to_value(parsed)?;
        ensure!(
            existing.as_ref().is_none_or(|existing| existing == &value),
            "durable parsed artifact differs from retained stage"
        );
        let changed = sqlx::query("UPDATE durable_page_attempts SET parsed=$2,state=CASE WHEN state='completed' THEN state ELSE 'parsed' END WHERE attempt_id=$1 AND state IN ('captured','parsed','completed')")
            .bind(&reservation.attempt_id).bind(value).execute(&mut *transaction).await?.rows_affected();
        ensure!(changed == 1, "parse cannot replace a failed attempt");
        transaction
            .commit()
            .await
            .context("committing durable parsed stage")
    }

    pub async fn durable_fail(
        &self,
        reservation: &PageReservation,
        failure: FailureRecord<'_>,
    ) -> Result<PageOutcome> {
        self.durable_fail_with_policy(reservation, failure, &RetryPolicy::default())
            .await
    }

    pub(crate) async fn durable_fail_with_policy(
        &self,
        reservation: &PageReservation,
        failure: FailureRecord<'_>,
        policy: &RetryPolicy,
    ) -> Result<PageOutcome> {
        policy.check(&reservation.request)?;
        let FailureRecord {
            class,
            detail,
            capture,
            stopped_origin,
            observed_at_ms: now_ms,
        } = failure;
        ensure!(
            capture.is_none_or(
                |response| !crate::transport::access_stop_status(response.status)
                    && !crate::transport::denied_capture(response)
            ) || class == FailureClass::Blocked,
            "access stop cannot become a transient retry"
        );
        ensure!(
            !detail.is_empty() && detail.len() <= 8192,
            "invalid durable failure detail"
        );
        let mut transaction = self.pool.begin().await?;
        lock_reservation(&mut transaction, reservation).await?;
        let existing: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT outcome FROM durable_page_attempts WHERE attempt_id=$1")
                .bind(&reservation.attempt_id)
                .fetch_one(&mut *transaction)
                .await?;
        if let Some(outcome) = existing {
            let outcome = serde_json::from_value(outcome)?;
            transaction.commit().await?;
            return Ok(outcome);
        }
        let outcome = failure_outcome(reservation, class, detail, now_ms, policy)?;
        let job = Job {
            id: reservation.request.binding.page_id,
            target: reservation.request.target.clone(),
        };
        super::super::queue::fail(
            &mut transaction,
            &job,
            capture,
            detail,
            class == FailureClass::Blocked,
            stopped_origin,
        )
        .await?;
        let (state, due) = disposition(&outcome);
        sqlx::query(
            "UPDATE durable_page_attempts SET state='failed',outcome=$2 WHERE attempt_id=$1",
        )
        .bind(&reservation.attempt_id)
        .bind(serde_json::to_value(&outcome)?)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE durable_page_operations SET state=$2,retry_at_ms=$3,outcome=$4 WHERE operation_id=$1")
            .bind(&reservation.request.binding.operation_id).bind(state).bind(due).bind(serde_json::to_value(&outcome)?).execute(&mut *transaction).await?;
        super::super::finish(transaction, Ok(outcome)).await
    }
}

fn failure_outcome(
    reservation: &PageReservation,
    class: FailureClass,
    detail: &str,
    now_ms: i64,
    policy: &RetryPolicy,
) -> Result<PageOutcome> {
    let detail = detail.to_owned();
    match class {
        FailureClass::Blocked => Ok(PageOutcome::Blocked { detail }),
        FailureClass::Rejected => Ok(PageOutcome::Rejected { detail }),
        FailureClass::Transient => {
            let scope = &reservation.request.binding.scope;
            ensure!(
                reservation.ordinal <= policy.attempts(),
                "durable ordinal exceeds configured retry attempts"
            );
            let delay = if reservation.ordinal == policy.attempts() {
                scope.cooldown_ms
            } else {
                i64::from(reservation.ordinal)
                    .checked_mul(1000)
                    .context("retry delay overflow")?
            };
            let retry_at_ms = now_ms
                .checked_add(delay)
                .context("retry deadline overflow")?;
            if retry_at_ms >= scope.deadline_ms {
                return Ok(PageOutcome::Exhausted { detail });
            }
            Ok(PageOutcome::Deferred {
                retry_at_ms,
                cycle: reservation.cycle,
            })
        }
    }
}

const fn disposition(outcome: &PageOutcome) -> (&'static str, Option<i64>) {
    match outcome {
        PageOutcome::Deferred { retry_at_ms, .. } => ("deferred", Some(*retry_at_ms)),
        PageOutcome::Blocked { .. } => ("blocked", None),
        PageOutcome::Rejected { .. } => ("rejected", None),
        PageOutcome::Exhausted { .. } => ("exhausted", None),
        PageOutcome::Completed { .. } => ("completed", None),
    }
}
