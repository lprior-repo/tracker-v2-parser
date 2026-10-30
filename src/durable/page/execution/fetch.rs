use std::path::Path;

use anyhow::{Context, Result};

use super::super::{
    FailureClass, FailureRecord, FetchDecision, PageReservation, RetryPolicy, artifacts, failures,
    retained, stopped,
};
use crate::{acquisition::ScopeExclusion, database::Database, transport::Transport};

pub struct FetchPolicy<'a> {
    pub retry: &'a RetryPolicy,
    pub exclusion: Option<&'a ScopeExclusion>,
}

pub async fn fetch(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    transport: &mut Transport,
    now_ms: i64,
) -> Result<FetchDecision> {
    fetch_with_policy(
        database,
        root,
        reservation,
        transport,
        now_ms,
        FetchPolicy {
            retry: &RetryPolicy::default(),
            exclusion: None,
        },
    )
    .await
}

pub async fn fetch_with_policy(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    transport: &mut Transport,
    now_ms: i64,
    policy: FetchPolicy<'_>,
) -> Result<FetchDecision> {
    policy.retry.check(&reservation.request)?;
    let started = tokio::time::Instant::now();
    let (capture, _, outcome) = database.durable_stages(reservation).await?;
    if let Some(outcome) = outcome {
        return Ok(FetchDecision::Settled { outcome });
    }
    if let Some(stop) = stopped::load(root, reservation).await? {
        return stopped::record(database, reservation, &stop).await;
    }
    if let Some(capture) = capture.or(retained::load(root, reservation).await?) {
        let restored = artifacts::capture(root, &capture).await?;
        if let Err(failure) =
            crate::transport::prepare_response(reservation.request.target.kind, restored)
        {
            return failures::record_with_policy(
                database,
                root,
                reservation,
                failure,
                now_ms,
                policy.retry,
            )
            .await;
        }
        database
            .durable_record_capture(reservation, &capture)
            .await?;
        return Ok(FetchDecision::Captured { capture });
    }
    if let Some(exclusion) = policy.exclusion {
        return Ok(FetchDecision::Settled {
            outcome: database
                .durable_exclude_reserved(reservation, exclusion)
                .await?,
        });
    }
    if now_ms >= reservation.request.binding.scope.deadline_ms {
        let outcome = database
            .durable_fail_with_policy(
                reservation,
                FailureRecord {
                    class: FailureClass::Transient,
                    detail: "durable fetch deadline reached",
                    capture: None,
                    stopped_origin: None,
                    observed_at_ms: now_ms,
                },
                policy.retry,
            )
            .await?;
        return Ok(FetchDecision::Settled { outcome });
    }
    transport.set_wire_context(
        &reservation.request.binding.operation_id,
        &reservation.attempt_id,
    )?;
    let fetched = transport.fetch(&reservation.request.target).await;
    let completed_at_ms = completion_time(now_ms, started.elapsed())?;
    match fetched {
        Ok(capture) => {
            super::retain_response_with_policy(
                database,
                root,
                reservation,
                capture,
                completed_at_ms,
                policy.retry,
            )
            .await
        }
        Err(failure) => {
            failures::record_with_policy(
                database,
                root,
                reservation,
                failure,
                completed_at_ms,
                policy.retry,
            )
            .await
        }
    }
}

fn completion_time(now_ms: i64, elapsed: std::time::Duration) -> Result<i64> {
    now_ms
        .checked_add(
            i64::try_from(elapsed.as_millis()).context("fetch duration exceeds clock bound")?,
        )
        .context("fetch completion clock overflow")
}
