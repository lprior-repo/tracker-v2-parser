use super::{Job, JobClass, attempt_limit};
use crate::model::Source;
use anyhow::{Context, Result};
use sqlx::PgPool;

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
    durable: Option<(&str, i64)>,
) -> Result<Option<Job>> {
    let row: Option<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT p.id,p.cursor FROM scrape_pages p WHERE p.status IN ('pending','failed') AND ($6::text IS NOT NULL OR p.attempts < $2) \
         AND ($1::text IS NULL OR p.source=$1) \
         AND NOT (p.cursor->>'kind'='contacts' AND p.url ~ '^https://schools[.]wiaawi[.]org/' \
           AND split_part(p.url,'?',1) <> 'https://schools.wiaawi.org/Directory/School/GetDirectorySchool') \
         AND (p.cursor->>'kind' <> 'athlete' OR cardinality($5::integer[]) = 0 OR NOT EXISTS ( \
           SELECT 1 FROM athlete_sources profile JOIN athletes athlete ON athlete.id=profile.athlete_id \
           WHERE profile.source=p.source AND profile.source_id=p.cursor->'context'->>'athlete_id' \
           AND COALESCE(profile.graduation_year,athlete.graduation_year) IS NOT NULL \
           AND NOT (COALESCE(profile.graduation_year,athlete.graduation_year)=ANY($5)))) \
         AND (cardinality($4::text[]) = 0 OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($4) \
           OR (p.source='athleticnet' AND p.cursor->>'kind'='directory' \
             AND p.cursor->'context'->>'state' IS NULL \
             AND p.cursor->'context'->>'subject_id'=array_to_string($4,','))) \
         AND (p.cursor->>'kind' <> 'contacts' OR NOT EXISTS ( \
           SELECT 1 FROM scrape_pages blocked CROSS JOIN LATERAL unnest(ARRAY[blocked.url,blocked.cursor->>'access_stop_url']) AS stop(url) WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' \
           AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')), \
             CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = \
           regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), \
             CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,''))) \
         AND ($6::text IS NULL OR NOT EXISTS (SELECT 1 FROM durable_page_operations d WHERE d.page_id=p.id AND d.run_generation=$6 AND (d.state IN ('completed','blocked','rejected','exhausted') OR (d.state='deferred' AND d.retry_at_ms > $7)))) \
         ORDER BY (p.attempts = 0) DESC, \
         CASE p.cursor->>'kind' \
           WHEN 'athlete' THEN ($3 <> 'profile')::int \
           WHEN 'roster' THEN ($3 <> 'roster')::int \
           WHEN 'contacts' THEN ($3 <> 'contacts')::int \
           ELSE ($3 <> 'bulk')::int END, \
         CASE p.cursor->>'kind' WHEN 'athlete' THEN 0 WHEN 'roster' THEN 1 WHEN 'contacts' THEN 2 ELSE 3 END, \
         CASE WHEN p.cursor->>'kind'='contacts' THEN \
           CASE p.cursor->'context'->>'subject_id' WHEN 'contacts-depth:2' THEN 0 WHEN 'contacts-depth:1' THEN 1 ELSE 2 END \
           ELSE 0 END, p.id LIMIT 1",
    )
    .bind(source.map(Source::as_str))
    .bind(attempt_limit(max_attempts)?)
    .bind(preferred.as_str())
    .bind(states)
    .bind(years)
    .bind(durable.map(|(generation,_)| generation))
    .bind(durable.map(|(_,now)| now))
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
