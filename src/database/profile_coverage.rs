use anyhow::{Context, Result, ensure};

use crate::model::Source;

use super::{Database, Job};

#[derive(Debug)]
pub(super) enum CoverageMode<'a> {
    Ready,
    Exhausted,
    Durable {
        generation: &'a str,
        now_ms: i64,
        owned: &'a [i64],
        allow_deferred: bool,
    },
}

impl Database {
    #[tracing::instrument(skip_all)]
    pub(crate) async fn prepare_profile_coverage(
        &self,
        config: &crate::config::Config,
        scope: &crate::durable::page::PageScope,
    ) -> Result<()> {
        if !config.enabled_sources().contains(&Source::MileSplit)
            || !matches!(
                config.acquisition,
                crate::acquisition::AcquisitionPolicy::PublicResults { .. }
            )
        {
            return Ok(());
        }
        super::coverage_scheduler::prepare(
            self,
            super::coverage_scheduler::Request {
                states: &config.states,
                years: &config.graduation_years,
                generation: Some(&scope.run_generation),
                max_attempts: i32::try_from(config.limits.max_attempts)?,
                contacts: config.acquisition.includes_contacts(),
                now_ms: 0,
                owned: &[],
                allow_deferred: true,
                exhausted: false,
            },
        )
        .await
    }

    #[tracing::instrument(skip(self, states, years))]
    pub async fn next_profile_coverage_job(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        states: &[String],
        years: &[i32],
        include_contacts: bool,
    ) -> Result<Option<Job>> {
        self.profile_coverage_candidate(
            source,
            max_attempts,
            states,
            years,
            include_contacts,
            CoverageMode::Ready,
        )
        .await
    }

    #[tracing::instrument(skip(self, states, years))]
    pub async fn check_profile_coverage_exhausted(
        &self,
        source: Source,
        max_attempts: u32,
        states: &[String],
        years: &[i32],
        include_contacts: bool,
    ) -> Result<()> {
        let exhausted = self
            .profile_coverage_candidate(
                Some(source),
                max_attempts,
                states,
                years,
                include_contacts,
                CoverageMode::Exhausted,
            )
            .await?;
        ensure!(
            exhausted.is_none(),
            "{} has exhausted unfinished school-chain coverage checkpoints",
            source.as_str()
        );
        Ok(())
    }

    #[tracing::instrument(skip(self, states, years))]
    pub(super) async fn profile_coverage_candidate(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        states: &[String],
        years: &[i32],
        include_contacts: bool,
        mode: CoverageMode<'_>,
    ) -> Result<Option<Job>> {
        ensure!(
            (1..=3).contains(&max_attempts),
            "maximum page attempts must be between 1 and 3"
        );
        let exhausted = matches!(mode, CoverageMode::Exhausted);
        let durable = match mode {
            CoverageMode::Durable {
                generation,
                now_ms,
                owned,
                allow_deferred,
            } => Some((generation, now_ms, owned, allow_deferred)),
            CoverageMode::Ready | CoverageMode::Exhausted => None,
        };
        if source.is_some_and(|source| source != Source::MileSplit) {
            return Ok(None);
        }
        super::coverage_scheduler::select(
            self,
            super::coverage_scheduler::Request {
                states,
                years,
                max_attempts: i32::try_from(max_attempts).context("converting maximum attempts")?,
                contacts: include_contacts,
                exhausted,
                generation: durable.map(|(generation, _, _, _)| generation),
                now_ms: durable.map_or(0, |(_, now, _, _)| now),
                owned: durable.map_or(&[][..], |(_, _, owned, _)| owned),
                allow_deferred: durable.is_none_or(|(_, _, _, allow)| allow),
            },
        )
        .await
    }
}
