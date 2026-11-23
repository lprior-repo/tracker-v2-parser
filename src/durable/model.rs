use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

mod crawl_request;
pub use crawl_request::CrawlRequest;

pub(crate) const BATCH_LIMIT_REACHED: &str =
    "bounded batch count reached; retained checkpoints are resumable";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadlineExtensionAuthorization {
    pub previous_deadline_ms: i64,
    pub effective_deadline_ms: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_deadlines_ms: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerVersion {
    RollingV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlPhase {
    SchoolChains,
    ContactRefresh,
    PublicResults,
    Complete,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchProgress {
    pub selected: u32,
    pub completed: u32,
    pub deferred: u32,
    pub blocked: u32,
    pub rejected: u32,
    pub exhausted: u32,
    pub last_page_id: Option<i64>,
    pub queue: QueueState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    #[default]
    Processing,
    NoEligiblePage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrawlReport {
    pub generation: String,
    pub batch: u32,
    pub phase: CrawlPhase,
    pub progress: BatchProgress,
    pub export: Option<operations::OperationResult>,
    pub continuation: Option<String>,
    pub stopped_reason: Option<String>,
}

use super::{operations, page::PageOutcome};

impl CrawlPhase {
    #[must_use]
    pub const fn scope_name(self) -> &'static str {
        match self {
            Self::SchoolChains => "schools",
            Self::ContactRefresh => "contacts",
            Self::PublicResults => "results",
            Self::Complete => "complete",
        }
    }
}

impl BatchProgress {
    pub fn record(mut self, page_id: i64, outcome: &PageOutcome) -> Result<Self> {
        self.selected = self
            .selected
            .checked_add(1)
            .context("selected page count overflow")?;
        self.last_page_id = Some(page_id);
        let count = match outcome {
            PageOutcome::Completed { .. } => &mut self.completed,
            PageOutcome::Deferred { .. } => &mut self.deferred,
            PageOutcome::Blocked { .. } => &mut self.blocked,
            PageOutcome::Rejected { .. } => &mut self.rejected,
            PageOutcome::Exhausted { .. } => &mut self.exhausted,
        };
        *count = count
            .checked_add(1)
            .context("page outcome count overflow")?;
        Ok(self)
    }
}

pub fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "identifier must contain 1..128 ASCII letters, digits, hyphens or underscores"
    );
    Ok(())
}

pub fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

pub fn page_config_digest(config: &Config) -> Result<String> {
    let mut acquisition = config.clone();
    acquisition.export.review_sheet = false;
    digest(&acquisition)
}

pub fn delay_until(now_ms: i64, due_ms: i64) -> Result<Duration> {
    let delay = due_ms.saturating_sub(now_ms).max(0);
    Ok(Duration::from_millis(u64::try_from(delay)?))
}

#[cfg(test)]
mod tests {
    use super::{Config, CrawlPhase, CrawlRequest, page_config_digest};
    use anyhow::Result;

    #[test]
    fn review_false_page_digest_matches_the_preexisting_page_value_digest() -> Result<()> {
        let mut config = Config::default();
        let original = crate::durable::page::value_digest(&config)?;
        assert_eq!(page_config_digest(&config)?, original);
        config.export.review_sheet = true;
        assert_ne!(crate::durable::page::value_digest(&config)?, original);
        assert_eq!(page_config_digest(&config)?, original);
        Ok(())
    }

    #[test]
    fn crawl_request_without_extension_decodes_as_legacy_input() -> Result<()> {
        let legacy = CrawlRequest::new(
            "legacy-generation".into(),
            Config::default(),
            100_000,
            60_000,
            1,
            CrawlPhase::SchoolChains,
            Vec::new(),
        )?;
        let value = serde_json::to_value(&legacy)?;
        assert!(value.get("deadline_extension").is_none());
        let decoded: CrawlRequest = serde_json::from_value(value)?;
        assert!(decoded.deadline_extension().is_none());
        Ok(())
    }
}
