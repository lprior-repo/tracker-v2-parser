use anyhow::{Context, Result, ensure};

use crate::{model::Source, scope::LOWER_48};

use super::{Database, Job};

#[derive(Debug)]
pub(super) enum CoverageMode<'a> {
    Ready,
    Exhausted,
    Durable {
        generation: &'a str,
        now_ms: i64,
        owned: &'a [i64],
    },
}

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
            } => Some((generation, now_ms, owned)),
            CoverageMode::Ready | CoverageMode::Exhausted => None,
        };
        let row: Option<(i64, serde_json::Value)> =
            sqlx::query_as(include_str!("profile_coverage.sql"))
                .bind(source.map(Source::as_str))
                .bind(i32::try_from(max_attempts).context("converting maximum attempts")?)
                .bind(states)
                .bind(years)
                .bind(exhausted)
                .bind(LOWER_48.as_slice())
                .bind(include_contacts)
                .bind(durable.map(|(generation, _, _)| generation))
                .bind(durable.map(|(_, now, _)| now))
                .bind(durable.map_or(&[][..], |(_, _, owned)| owned))
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
