use std::ops::ControlFlow;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};

use super::{PageRequest, PageScope, ReservationDecision};
use crate::{config::Config, database::Database};

#[derive(Clone, Copy)]
pub enum Scheduling {
    Resume,
    SchoolSweep,
    PublicBulkTurn,
}

impl Scheduling {
    pub fn for_batch_turn(config: &Config, batch: u32, turn: usize) -> Result<Self> {
        let offset = usize::try_from(batch)
            .context("scheduling batch exceeds addressable range")?
            .checked_mul(config.limits.max_pages)
            .and_then(|offset| offset.checked_add(turn))
            .context("scheduling turn overflow")?;
        Ok(Self::for_turn(config, offset))
    }

    pub const fn for_turn(config: &Config, turn: usize) -> Self {
        let interval = match config.limits.public_results_every {
            Some(value) => value.get(),
            None => 8,
        };
        let public_turn = match turn.checked_rem(interval) {
            Some(remainder) => remainder == interval.saturating_sub(1),
            None => false,
        };
        if config.acquisition.profile_coverage_only() {
            Self::SchoolSweep
        } else if public_turn {
            Self::PublicBulkTurn
        } else {
            Self::Resume
        }
    }
}

pub async fn select(
    database: &Database,
    config: &Config,
    scope: &PageScope,
    now_ms: i64,
) -> Result<Option<PageRequest>> {
    select_excluding(database, config, scope, now_ms, &[]).await
}

pub async fn select_excluding(
    database: &Database,
    config: &Config,
    scope: &PageScope,
    now_ms: i64,
    owned: &[i64],
) -> Result<Option<PageRequest>> {
    select_scheduled(database, config, scope, now_ms, owned, Scheduling::Resume).await
}

pub async fn select_scheduled(
    database: &Database,
    config: &Config,
    scope: &PageScope,
    now_ms: i64,
    owned: &[i64],
    scheduling: Scheduling,
) -> Result<Option<PageRequest>> {
    ensure!(
        owned.len() <= 3 && owned.iter().all(|id| *id > 0),
        "invalid pipeline ownership window"
    );
    super::binding::validate_scope(scope)?;
    ensure!(
        crate::durable::model::page_config_digest(config)? == scope.config_sha256,
        "durable configuration binding mismatch"
    );
    if now_ms >= scope.deadline_ms {
        return Ok(None);
    }
    super::binding::deadline(scope, now_ms)?;
    crate::runner::durable::check_stops(database, config).await?;
    let progress = stream::iter((0..100).map(Ok::<_, anyhow::Error>))
        .try_fold(ControlFlow::Continue(()), |progress, _| async move {
            match progress {
                ControlFlow::Break(_) => Ok(progress),
                ControlFlow::Continue(()) => {
                    policy_candidate(database, config, scope, now_ms, owned, scheduling).await
                }
            }
        })
        .await?;
    match progress {
        ControlFlow::Break(selected) => Ok(selected),
        ControlFlow::Continue(()) => Err(anyhow::Error::from(crate::durable::AcquisitionBusy))
            .context(
                "bounded policy exclusion scan reached 100 candidates; committed progress retained",
            ),
    }
}

async fn policy_candidate(
    database: &Database,
    config: &Config,
    scope: &PageScope,
    now_ms: i64,
    owned: &[i64],
    scheduling: Scheduling,
) -> Result<ControlFlow<Option<PageRequest>>> {
    let Some(job) = database
        .next_durable_page_scheduled(config, scope, now_ms, owned, scheduling)
        .await?
    else {
        return Ok(ControlFlow::Break(None));
    };
    let request = super::request(job, scope)?;
    let Some(exclusion) = crate::acquisition::ScopeExclusion::bind(config, &request)? else {
        return Ok(ControlFlow::Break(Some(request)));
    };
    match database.durable_exclude(&request, &exclusion).await? {
        ReservationDecision::Reserved { .. } => Ok(ControlFlow::Break(Some(request))),
        ReservationDecision::Settled { .. } => Ok(ControlFlow::Continue(())),
    }
}

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod exclusion_scan_tests;
#[cfg(test)]
mod reviewed_legacy_tests;
#[cfg(test)]
mod tests;

pub async fn reserve(
    database: &Database,
    request: &PageRequest,
    now_ms: i64,
) -> Result<ReservationDecision> {
    database.durable_reserve(request, now_ms).await
}

pub async fn reserve_configured(
    database: &Database,
    config: &Config,
    request: &PageRequest,
    now_ms: i64,
) -> Result<ReservationDecision> {
    let policy = super::RetryPolicy::configured(config, request)?;
    if let Some(exclusion) = crate::acquisition::ScopeExclusion::bind(config, request)? {
        return database.durable_exclude(request, &exclusion).await;
    }
    let permit = crate::acquisition::LegacyFailurePermit::bind(config, request)?;
    let index = super::IndexRefreshPermit::bind(config, request)?;
    database
        .durable_reserve_configured(request, now_ms, permit.as_ref(), index.as_ref(), &policy)
        .await
}
