use anyhow::{Context, Result, ensure};

use crate::{model::Source, scope::LOWER_48};

use super::{Database, Job};

impl Database {
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
            false,
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
                true,
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
    async fn profile_coverage_candidate(
        &self,
        source: Option<Source>,
        max_attempts: u32,
        states: &[String],
        years: &[i32],
        include_contacts: bool,
        exhausted: bool,
    ) -> Result<Option<Job>> {
        ensure!(
            (1..=3).contains(&max_attempts),
            "maximum page attempts must be between 1 and 3"
        );
        let row: Option<(i64, serde_json::Value)> =
            sqlx::query_as(include_str!("profile_coverage.sql"))
                .bind(source.map(Source::as_str))
                .bind(i32::try_from(max_attempts).context("converting maximum attempts")?)
                .bind(states)
                .bind(years)
                .bind(exhausted)
                .bind(LOWER_48.as_slice())
                .bind(include_contacts)
                .fetch_optional(&self.pool)
                .await
                .context("selecting published school-chain coverage checkpoint")?;
        row.map(|(id, target)| {
            Ok(Job {
                id,
                target: serde_json::from_value(target)
                    .context("decoding school-chain coverage target")?,
            })
        })
        .transpose()
    }
}
