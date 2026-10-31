use std::path::Path;

use anyhow::{Result, ensure};

use super::{FetchDecision, PageOutcome, PageRequest, PageReservation};
use crate::{config::Config, database::Database, transport::Transport};

pub struct RetryPolicy {
    attempts: i32,
    config_sha256: Option<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            config_sha256: None,
        }
    }
}

impl RetryPolicy {
    pub fn configured(config: &Config, request: &PageRequest) -> Result<Self> {
        let mut canonical = config.clone();
        canonical.validate()?;
        let digest = crate::durable::model::page_config_digest(config)?;
        ensure!(
            crate::durable::model::page_config_digest(&canonical)? == digest
                && digest == request.binding.scope.config_sha256,
            "durable configuration binding mismatch"
        );
        Ok(Self {
            attempts: i32::try_from(config.limits.max_attempts)?,
            config_sha256: Some(digest),
        })
    }

    pub fn check(&self, request: &PageRequest) -> Result<()> {
        ensure!(
            self.config_sha256
                .as_ref()
                .is_none_or(|digest| digest == &request.binding.scope.config_sha256),
            "durable retry policy binding mismatch"
        );
        Ok(())
    }

    pub(crate) fn exhausted(&self, attempts: i64, detail: &str) -> PageOutcome {
        PageOutcome::Exhausted {
            detail: format!(
                "acquisition attempt budget exhausted ({attempts}/{}); {detail}",
                self.attempts
            ),
        }
    }

    pub const fn attempts(&self) -> i32 {
        self.attempts
    }
}

pub async fn fetch_configured(
    database: &Database,
    root: &Path,
    reservation: &PageReservation,
    transport: &mut Transport,
    now_ms: i64,
    config: &Config,
) -> Result<FetchDecision> {
    let policy = RetryPolicy::configured(config, &reservation.request)?;
    let exclusion = crate::acquisition::ScopeExclusion::bind(config, &reservation.request)?;
    super::execution::fetch_with_policy(
        database,
        root,
        reservation,
        transport,
        now_ms,
        super::execution::FetchPolicy {
            retry: &policy,
            exclusion: exclusion.as_ref(),
        },
    )
    .await
}

#[cfg(test)]
mod exclusion_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_tests;

#[cfg(test)]
mod budget_tests;

#[cfg(test)]
mod budget_wire_tests;
