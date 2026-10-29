use super::{Job, JobClass, attempt_limit};
use crate::{model::Source, scope::LOWER_48};
use anyhow::{Context, Result};
use sqlx::PgPool;

#[derive(Debug, Clone, Copy)]
pub(in crate::database) enum FallbackScope {
    All,
    MileSplitPublicDiscovery,
}

#[derive(Clone, Copy)]
pub(in crate::database) struct DurableSelection<'a> {
    pub generation: &'a str,
    pub now_ms: i64,
    pub owned: &'a [i64],
    pub scope: FallbackScope,
}

#[tracing::instrument(skip(pool, states))]
pub(in crate::database) async fn next(
    pool: &PgPool,
    source: Option<Source>,
    max_attempts: u32,
    preferred: JobClass,
    states: &[String],
) -> Result<Option<Job>> {
    next_for_cohort(pool, source, max_attempts, preferred, states, &[]).await
}

#[tracing::instrument(skip(pool, states, years))]
pub(in crate::database) async fn next_for_cohort(
    pool: &PgPool,
    source: Option<Source>,
    max_attempts: u32,
    preferred: JobClass,
    states: &[String],
    years: &[i32],
) -> Result<Option<Job>> {
    next_for_cohort_in(pool, source, max_attempts, preferred, states, years, None).await
}

pub(in crate::database) async fn next_for_cohort_in(
    pool: &PgPool,
    source: Option<Source>,
    max_attempts: u32,
    preferred: JobClass,
    states: &[String],
    years: &[i32],
    durable: Option<DurableSelection<'_>>,
) -> Result<Option<Job>> {
    let row: Option<(i64, serde_json::Value)> = sqlx::query_as(include_str!("selection.sql"))
        .bind(source.map(Source::as_str))
        .bind(attempt_limit(max_attempts)?)
        .bind(preferred.as_str())
        .bind(states)
        .bind(years)
        .bind(durable.map(|selection| selection.generation))
        .bind(durable.map(|selection| selection.now_ms))
        .bind(durable.map_or(&[][..], |selection| selection.owned))
        .bind(durable.is_some_and(|selection| {
            matches!(selection.scope, FallbackScope::MileSplitPublicDiscovery)
        }))
        .bind(LOWER_48.as_slice())
        .fetch_optional(pool)
        .await?;
    row.map(|(id, target)| {
        Ok(Job {
            id,
            target: serde_json::from_value(target).context("decoding checkpoint target")?,
        })
    })
    .transpose()
}
