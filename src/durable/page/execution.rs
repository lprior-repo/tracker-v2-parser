mod fetch;

pub use fetch::fetch;
pub(super) use fetch::{FetchPolicy, fetch_with_policy};

use std::{borrow::Cow, path::Path};

use anyhow::{Context, Result, ensure};

use super::{
    CaptureRef, FailureClass, FailureRecord, FetchDecision, PageOutcome, PageReservation,
    ParseDecision, ParsedRef, RetryPolicy, artifacts,
};
use crate::{
    database::Database,
    model::{Capture, PageKind, ParsedPage, Source},
};

pub(super) async fn retain_response(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    capture: Capture,
    now_ms: i64,
) -> Result<FetchDecision> {
    retain_response_with_policy(
        database,
        root,
        reservation,
        capture,
        now_ms,
        &RetryPolicy::default(),
    )
    .await
}

async fn retain_response_with_policy(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    capture: Capture,
    now_ms: i64,
    policy: &RetryPolicy,
) -> Result<FetchDecision> {
    match crate::transport::sanitize_response(capture) {
        Ok(capture) => retain(database, root, reservation, capture, now_ms)
            .await
            .map(|capture| FetchDecision::Captured { capture }),
        Err(failure) => {
            super::failures::record_with_policy(
                database,
                root,
                reservation,
                failure,
                now_ms,
                policy,
            )
            .await
        }
    }
}

pub(super) async fn retain(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    capture: Capture,
    now_ms: i64,
) -> Result<CaptureRef> {
    let capture = crate::transport::redact_capture(capture)?;
    if let Some(existing) = super::retained::load(root, reservation).await? {
        ensure!(
            existing.url == capture.url
                && existing.status == capture.status
                && existing.content_type == capture.content_type
                && existing.body.sha256 == super::body_digest(&capture.body),
            "offline capture differs from retained attempt"
        );
        database
            .durable_record_capture(reservation, &existing)
            .await?;
        return Ok(existing);
    }
    ensure!(
        capture.url.len() <= 8192 && capture.content_type.len() <= 1024,
        "capture metadata exceeds durable bound"
    );
    let body = artifacts::write(root, capture.body, crate::transport::MAX_BODY_BYTES).await?;
    let reference = CaptureRef {
        body,
        url: capture.url,
        status: capture.status,
        content_type: capture.content_type,
        retained_at_ms: now_ms,
    };
    super::retained::save(root, reservation, &reference).await?;
    database
        .durable_record_capture(reservation, &reference)
        .await?;
    Ok(reference)
}

pub async fn parse(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    reference: &CaptureRef,
    now_ms: i64,
) -> Result<ParseDecision> {
    let (saved, parsed, outcome) = database.durable_stages(reservation).await?;
    ensure!(
        saved.as_ref() == Some(reference),
        "parse input differs from recorded capture"
    );
    if let Some(outcome) = outcome {
        return Ok(ParseDecision::Settled { outcome });
    }
    if let Some(parsed) = parsed {
        artifacts::read(root, &parsed.body, artifacts::MAX_PARSED_BYTES).await?;
        return Ok(ParseDecision::Parsed { parsed });
    }
    let capture = artifacts::capture(root, reference).await?;
    let target = &reservation.request.target;
    let adjusted = if target.kind == PageKind::Contacts && target.url != capture.url {
        let mut value = target.clone();
        value.url.clone_from(&capture.url);
        Cow::Owned(value)
    } else {
        Cow::Borrowed(target)
    };
    let result = std::str::from_utf8(&capture.body)
        .context("source response is not UTF-8")
        .and_then(|body| {
            if crate::durable::wiaa::is_index(&adjusted) {
                crate::durable::wiaa::parse_index(&adjusted, body)
            } else {
                crate::sources::parse(&adjusted, body)
            }
        });
    let mut page = match result {
        Ok(page) => page,
        Err(error) => {
            let class = if error.is::<crate::sources::AccessDenied>()
                || error.is::<crate::sources::contacts::AccessDenied>()
            {
                FailureClass::Blocked
            } else {
                FailureClass::Rejected
            };
            let outcome = database
                .durable_fail(
                    reservation,
                    FailureRecord {
                        class,
                        detail: &super::failures::bounded_detail(&format!("{error:#}")),
                        capture: Some(&capture),
                        stopped_origin: None,
                        observed_at_ms: now_ms,
                    },
                )
                .await?;
            return Ok(ParseDecision::Settled { outcome });
        }
    };
    if target.source == Source::MileSplit && target.kind == PageKind::Athlete {
        artifacts::attach(root, reference, &target.url, &mut page)?;
    }
    let bytes = serde_json::to_vec(&page)?;
    let body = artifacts::write(root, bytes, artifacts::MAX_PARSED_BYTES).await?;
    let parsed = ParsedRef {
        body,
        capture_sha256: super::capture_digest(reference)?,
    };
    database.durable_record_parse(reservation, &parsed).await?;
    Ok(ParseDecision::Parsed { parsed })
}

pub async fn persist(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    reference: &CaptureRef,
    parsed: &ParsedRef,
) -> Result<PageOutcome> {
    let capture = artifacts::capture(root, reference).await?;
    let bytes = artifacts::read(root, &parsed.body, artifacts::MAX_PARSED_BYTES).await?;
    let page: ParsedPage =
        serde_json::from_slice(&bytes).context("decoding durable parsed page")?;
    database
        .durable_persist(reservation, reference, parsed, &capture, &page)
        .await
}
