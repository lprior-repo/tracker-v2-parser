use anyhow::{Result, ensure};

use super::{PageRequest, PageScope, ReservationDecision};
use crate::{config::Config, database::Database};

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
        .next_durable_page_excluding(config, scope, now_ms, owned)
        .await?
        .map(|job| super::request(job, scope))
        .transpose()
}

pub async fn reserve(
    database: &Database,
    request: &PageRequest,
    now_ms: i64,
) -> Result<ReservationDecision> {
    database.durable_reserve(request, now_ms).await
}
