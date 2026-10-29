use anyhow::Result;
use futures::TryStreamExt;

use super::Database;
use crate::{
    config::Config,
    database::{Job, JobClass},
    durable::page::PageScope,
    model::Source,
};

impl Database {
    pub async fn next_durable_page(
        &self,
        config: &Config,
        scope: &PageScope,
        now_ms: i64,
    ) -> Result<Option<Job>> {
        self.next_durable_page_excluding(config, scope, now_ms, &[])
            .await
    }

    pub async fn next_durable_page_excluding(
        &self,
        config: &Config,
        scope: &PageScope,
        now_ms: i64,
        owned: &[i64],
    ) -> Result<Option<Job>> {
        let (_, found) = futures::stream::iter(
            config
                .enabled_sources()
                .into_iter()
                .map(Ok::<_, anyhow::Error>),
        )
        .try_fold((self, None), |(database, found), source| async move {
            if found.is_some() {
                return Ok((database, found));
            }
            let job = if source == Source::MileSplit
                && matches!(
                    &config.acquisition,
                    crate::acquisition::AcquisitionPolicy::PublicResults { .. }
                ) {
                database
                    .profile_coverage_candidate(
                        Some(source),
                        config.limits.max_attempts,
                        &config.states,
                        &config.graduation_years,
                        config.acquisition.includes_contacts(),
                        super::super::profile_coverage::CoverageMode::Durable {
                            generation: &scope.run_generation,
                            now_ms,
                            owned,
                        },
                    )
                    .await?
            } else {
                None
            };
            let job = if job.is_none() && !config.acquisition.profile_coverage_only() {
                super::super::queue::next_for_cohort_in(
                    &database.pool,
                    Some(source),
                    config.limits.max_attempts,
                    JobClass::Profile,
                    &config.states,
                    &config.graduation_years,
                    Some((&scope.run_generation, now_ms, owned)),
                )
                .await?
            } else {
                job
            };
            Ok((database, job))
        })
        .await?;
        Ok(found)
    }
}
