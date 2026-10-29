use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrawlRequest {
    pub generation: String,
    pub config: Config,
    pub deadline_ms: i64,
    pub cooldown_ms: i64,
    pub batch: u32,
    pub max_batches: u32,
    pub phase: CrawlPhase,
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

use super::{
    operations,
    page::{PageOutcome, PageScope},
};

impl CrawlRequest {
    pub fn validate(&self, now_ms: i64) -> Result<()> {
        identifier(&self.generation)?;
        ensure!(
            self.generation.len() <= 100,
            "crawl generation exceeds 100 bytes"
        );
        let mut config = self.config.clone();
        config.validate()?;
        ensure!(
            digest(&config)? == digest(&self.config)?,
            "crawl configuration must be canonical"
        );
        ensure!(
            self.batch < self.max_batches && self.max_batches <= 10_000,
            "invalid bounded batch range"
        );
        ensure!(
            (15_000..=3_600_000).contains(&self.cooldown_ms),
            "cooldown must be 15 seconds to one hour"
        );
        let remaining = self
            .deadline_ms
            .checked_sub(now_ms)
            .context("invalid crawl deadline")?;
        ensure!(
            (0..=86_400_000).contains(&remaining),
            "crawl deadline must be within 24 hours"
        );
        Ok(())
    }

    #[must_use]
    pub fn workflow_key(&self) -> String {
        format!("{}-{:06}", self.generation, self.batch)
    }

    pub fn page_scope(&self) -> Result<PageScope> {
        Ok(PageScope {
            run_generation: format!("{}-{}", self.generation, self.phase.scope_name()),
            config_sha256: digest(&self.config)?,
            deadline_ms: self.deadline_ms,
            cooldown_ms: self.cooldown_ms,
        })
    }

    pub fn next(&self, phase: CrawlPhase) -> Result<Self> {
        let batch = self
            .batch
            .checked_add(1)
            .context("crawl batch counter overflow")?;
        let mut next = self.clone();
        next.batch = batch;
        next.phase = phase;
        if phase == CrawlPhase::PublicResults {
            next.config.acquisition.set_profile_coverage_only(false)?;
        }
        Ok(next)
    }
}

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

pub fn delay_until(now_ms: i64, due_ms: i64) -> Result<Duration> {
    let delay = due_ms.saturating_sub(now_ms).max(0);
    Ok(Duration::from_millis(u64::try_from(delay)?))
}
