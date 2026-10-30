use anyhow::{Result, ensure};

use super::{PageRequest, PageScope, ReservationDecision};
use crate::{config::Config, database::Database};

#[derive(Clone, Copy)]
pub enum Scheduling {
    Resume,
    SchoolSweep,
    PublicBulkTurn,
}

impl Scheduling {
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
    database
        .next_durable_page_scheduled(config, scope, now_ms, owned, scheduling)
        .await?
        .map(|job| super::request(job, scope))
        .transpose()
}

#[cfg(test)]
mod tests;

pub async fn reserve(
    database: &Database,
    request: &PageRequest,
    now_ms: i64,
) -> Result<ReservationDecision> {
    database.durable_reserve(request, now_ms).await
}
