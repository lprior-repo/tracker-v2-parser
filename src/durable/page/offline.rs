use anyhow::Result;

use super::{
    FetchDecision, PageFiles, PageOutcome, PageRequest, PageScope, ParseDecision,
    ReservationDecision,
};
use crate::{
    database::Database,
    model::{Capture, Target},
};

pub async fn offline_replay(
    database: &Database,
    files: PageFiles<'_>,
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
            super::execution::retain_response(database, files, &reservation, capture, now_ms)
                .await?
        }
        Err(failure) => FetchDecision::Settled {
            outcome: database
                .durable_fail(
                    &reservation,
                    super::FailureRecord {
                        class: super::classify(&failure),
                        detail: &failure.detail,
                        capture: failure.capture(),
                        stopped_origin: failure.stopped_origin(),
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
    let parsed = match super::parse(database, files, &reservation, &reference, now_ms).await? {
        ParseDecision::Settled { outcome } => return Ok(outcome),
        ParseDecision::Parsed { parsed } => parsed,
    };
    super::persist(database, files, &reservation, &reference, &parsed).await
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
    super::request(database.replay_checkpoint(&target).await?, scope)
}
