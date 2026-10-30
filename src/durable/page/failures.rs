use super::{FailureClass, FailureRecord, FetchDecision, PageReservation, RetryPolicy};
use crate::{database::Database, model::Capture, transport::Failure};
use anyhow::Result;
use std::path::Path;

pub(super) async fn record_with_policy(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    failure: Failure,
    now_ms: i64,
    policy: &RetryPolicy,
) -> Result<FetchDecision> {
    if failure.blocked {
        super::stopped::save(root, reservation, &failure, now_ms).await?;
    }
    let mut detail = bounded_detail(&failure.detail);
    if let Some(capture) = failure.capture.as_ref() {
        let evidence = Capture {
            url: capture.url.clone(),
            status: capture.status,
            content_type: capture.content_type.clone(),
            body: capture.body.clone(),
        };
        if let Err(error) =
            super::execution::retain(database, root, reservation, evidence, now_ms).await
        {
            detail = bounded_detail(&format!("{detail}; retaining failure evidence: {error:#}"));
        }
    }
    let outcome = database
        .durable_fail_with_policy(
            reservation,
            FailureRecord {
                class: classify(&failure),
                detail: &detail,
                capture: failure.capture.as_ref(),
                stopped_origin: failure.stopped_origin.as_deref(),
                observed_at_ms: now_ms,
            },
            policy,
        )
        .await?;
    Ok(FetchDecision::Settled { outcome })
}

#[must_use]
pub fn classify(failure: &Failure) -> FailureClass {
    if failure.blocked {
        return FailureClass::Blocked;
    }
    if failure
        .capture
        .as_ref()
        .is_some_and(|capture| matches!(capture.status, 408 | 425 | 500 | 502 | 503 | 504))
    {
        return FailureClass::Transient;
    }
    if failure.capture.is_none() && (failure.retryable || transient_detail(&failure.detail)) {
        return FailureClass::Transient;
    }
    FailureClass::Rejected
}

pub(super) fn transient_detail(detail: &str) -> bool {
    let queue_saturation = matches!(
        detail,
        "browser interception failed: browser request admission budget exceeded: no available capacity"
            | "page cleanup also failed: browser acquisition incomplete: admission ended before quiescence: channel closed: browser interception failed: browser request admission budget exceeded: no available capacity"
    );
    let invalid_interception = detail.starts_with("browser interception failed:")
        && detail.contains("Error -32602: Invalid InterceptionId.");
    let detail = detail.to_ascii_lowercase();
    [
        "dns",
        "resolv",
        "lookup address",
        "nodename nor servname",
        "name or service not known",
        "connect",
        "timeout",
        "timed out",
        "temporarily unavailable",
        "network",
    ]
    .iter()
    .any(|marker| detail.contains(marker))
        || detail
            .split_ascii_whitespace()
            .zip(detail.split_ascii_whitespace().skip(1))
            .any(|(prefix, status)| {
                prefix == "http"
                    && matches!(
                        status.trim_end_matches([':', ';', ')']),
                        "408" | "425" | "500" | "502" | "503" | "504"
                    )
            })
        || invalid_interception
        || queue_saturation
}

pub(super) fn bounded_detail(detail: &str) -> String {
    detail.chars().take(2000).collect()
}
