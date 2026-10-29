use std::path::Path;

use anyhow::{Context, Result};

use super::{
    FetchDecision, PageOutcome, PageRequest, PageScope, ParseDecision, ReservationDecision,
};
use crate::{
    database::{Database, Job},
    model::{Capture, Target},
};

pub async fn offline_replay(
    database: &Database,
    root: &Path,
    scope: &PageScope,
    target: Target,
    capture: Capture,
) -> Result<PageOutcome> {
    let request = bind_target(database, target, scope).await?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let reservation = match database.durable_reserve_offline(&request, now_ms).await? {
        ReservationDecision::Settled { outcome } => return Ok(outcome),
        ReservationDecision::Reserved { reservation } => reservation,
    };
    let fetched = match crate::transport::prepare_response(reservation.request.target.kind, capture)
    {
        Ok(capture) => {
            super::execution::retain_response(database, root, &reservation, capture, now_ms).await?
        }
        Err(failure) => FetchDecision::Settled {
            outcome: database
                .durable_fail(
                    &reservation,
                    super::FailureRecord {
                        class: super::classify(&failure),
                        detail: &failure.detail,
                        capture: failure.capture.as_ref(),
                        stopped_origin: failure.stopped_origin.as_deref(),
                        observed_at_ms: now_ms,
                    },
                )
                .await?,
        },
    };
    let reference = match fetched {
        FetchDecision::Settled { outcome } => return Ok(outcome),
        FetchDecision::Captured { capture } => capture,
    };
    let parsed = match super::parse(database, root, &reservation, &reference, now_ms).await? {
        ParseDecision::Settled { outcome } => return Ok(outcome),
        ParseDecision::Parsed { parsed } => parsed,
    };
    super::persist(database, root, &reservation, &reference, &parsed).await
}

pub async fn bind_target(
    database: &Database,
    target: Target,
    scope: &PageScope,
) -> Result<PageRequest> {
    super::binding::validate_scope(scope)?;
    database
        .enqueue(std::slice::from_ref(&target), false)
        .await?;
    let (id,cursor): (i64,serde_json::Value) = sqlx::query_as("SELECT id,cursor FROM scrape_pages WHERE source=$1 AND ((cursor-'access_stop_url')=$2 OR (cursor->>'kind'='contacts' AND $2::jsonb->>'kind'='contacts' AND url=$2::jsonb->>'url' AND cursor->'context'->>'school_id'=$2::jsonb->'context'->>'school_id')) ORDER BY ((cursor-'access_stop_url')=$2) DESC,id LIMIT 1")
        .bind(target.source.as_str()).bind(serde_json::to_value(&target)?).fetch_one(database.pool()).await?;
    let target = serde_json::from_value(cursor).context("decoding bound durable target")?;
    super::request(Job { id, target }, scope)
}
