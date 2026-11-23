use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, de};

use super::{
    BATCH_LIMIT_REACHED, CrawlPhase, DeadlineExtensionAuthorization, SchedulerVersion, digest,
    identifier,
};
use crate::{
    config::Config,
    durable::{deadline_authority::DeadlineAuthority, page::PageScope, retry_lineage},
};

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct CrawlRequest(CrawlRequestFields);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "CrawlRequest", deny_unknown_fields)]
struct CrawlRequestFields {
    generation: String,
    config: Config,
    deadline_ms: i64,
    cooldown_ms: i64,
    batch: u32,
    max_batches: u32,
    phase: CrawlPhase,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inherited_retry_campaigns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduler_version: Option<SchedulerVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deadline_extension: Option<DeadlineExtensionAuthorization>,
}

impl<'de> Deserialize<'de> for CrawlRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(CrawlRequestFields::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl CrawlRequest {
    pub fn new(
        generation: String,
        config: Config,
        deadline_ms: i64,
        cooldown_ms: i64,
        max_batches: u32,
        phase: CrawlPhase,
        inherited_retry_campaigns: Vec<String>,
    ) -> Result<Self> {
        Self::from_fields(CrawlRequestFields {
            generation,
            config,
            deadline_ms,
            cooldown_ms,
            batch: 0,
            max_batches,
            phase,
            inherited_retry_campaigns,
            scheduler_version: None,
            deadline_extension: None,
        })
    }

    fn from_fields(fields: CrawlRequestFields) -> Result<Self> {
        identifier(&fields.generation)?;
        ensure!(
            fields.generation.len() <= 100,
            "crawl generation exceeds 100 bytes"
        );
        ensure!(
            fields.batch < fields.max_batches && fields.max_batches <= 10_000,
            "invalid bounded batch range"
        );
        ensure!(
            (15_000..=3_600_000).contains(&fields.cooldown_ms),
            "cooldown must be 15 seconds to one hour"
        );
        ensure!(fields.deadline_ms > 0, "invalid crawl deadline");
        if let Some(extension) = &fields.deadline_extension {
            validate_extension(fields.deadline_ms, extension)?;
        }
        Ok(Self(fields))
    }

    #[must_use]
    pub const fn generation(&self) -> &str {
        self.0.generation.as_str()
    }

    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.0.config
    }

    #[must_use]
    pub const fn deadline_ms(&self) -> i64 {
        self.0.deadline_ms
    }

    #[must_use]
    pub const fn cooldown_ms(&self) -> i64 {
        self.0.cooldown_ms
    }

    #[must_use]
    pub const fn batch(&self) -> u32 {
        self.0.batch
    }

    #[must_use]
    pub const fn max_batches(&self) -> u32 {
        self.0.max_batches
    }

    #[must_use]
    pub const fn phase(&self) -> CrawlPhase {
        self.0.phase
    }

    #[must_use]
    pub const fn inherited_retry_campaigns(&self) -> &[String] {
        self.0.inherited_retry_campaigns.as_slice()
    }

    #[must_use]
    pub const fn scheduler_version(&self) -> Option<SchedulerVersion> {
        self.0.scheduler_version
    }

    #[must_use]
    pub const fn deadline_extension(&self) -> Option<&DeadlineExtensionAuthorization> {
        self.0.deadline_extension.as_ref()
    }

    pub fn validate(&self, now_ms: i64) -> Result<()> {
        self.validate_lineage()?;
        let mut config = self.0.config.clone();
        config.validate()?;
        ensure!(
            digest(&config)? == digest(&self.0.config)?,
            "crawl configuration must be canonical"
        );
        let remaining = self
            .0
            .deadline_ms
            .checked_sub(now_ms)
            .context("invalid crawl deadline")?;
        ensure!(
            (0..=86_400_000).contains(&remaining),
            "crawl deadline must be within 24 hours"
        );
        Ok(())
    }

    fn validate_lineage(&self) -> Result<()> {
        retry_lineage::validate(&self.0.generation, &self.0.inherited_retry_campaigns)?;
        retry_lineage::validate_config(&self.0.config, &self.0.inherited_retry_campaigns)
    }

    #[must_use]
    pub fn workflow_key(&self) -> String {
        format!("{}-{:06}", self.0.generation, self.0.batch)
    }

    pub fn page_scope(&self) -> Result<PageScope> {
        self.validate_lineage()?;
        Ok(PageScope {
            run_generation: format!("{}-{}", self.0.generation, self.0.phase.scope_name()),
            config_sha256: super::page_config_digest(&self.0.config)?,
            deadline_ms: self.0.deadline_ms,
            cooldown_ms: self.0.cooldown_ms,
            inherited_retry_campaigns: self.0.inherited_retry_campaigns.clone(),
        })
    }

    fn next_batch(&self) -> Option<u32> {
        self.0
            .batch
            .checked_add(1)
            .filter(|&batch| batch < self.0.max_batches)
    }

    #[must_use]
    pub fn is_last_batch(&self) -> bool {
        self.next_batch().is_none()
    }

    pub fn next(&self, phase: CrawlPhase) -> Result<Self> {
        let batch = self.next_batch().context(BATCH_LIMIT_REACHED)?;
        let mut next = self.clone();
        next.0.batch = batch;
        next.0.phase = phase;
        if phase == CrawlPhase::PublicResults {
            next.0.config.acquisition.set_profile_coverage_only(false)?;
        }
        Ok(next)
    }

    #[must_use]
    pub const fn upgraded_scheduler(mut self) -> Self {
        self.0.scheduler_version = Some(SchedulerVersion::RollingV2);
        self
    }

    #[must_use]
    pub fn map_config(self, map: impl FnOnce(Config) -> Config) -> Self {
        Self(CrawlRequestFields {
            config: map(self.0.config),
            ..self.0
        })
    }

    pub fn with_deadline_extension(
        self,
        extension: DeadlineExtensionAuthorization,
    ) -> Result<Self> {
        validate_extension(self.0.deadline_ms, &extension)?;
        Ok(Self(CrawlRequestFields {
            deadline_extension: Some(extension),
            ..self.0
        }))
    }
}

fn validate_extension(deadline_ms: i64, extension: &DeadlineExtensionAuthorization) -> Result<()> {
    DeadlineAuthority::parse(extension)?;
    ensure!(
        extension.effective_deadline_ms == deadline_ms,
        "crawl deadline extension must explicitly authorize the request deadline"
    );
    Ok(())
}
