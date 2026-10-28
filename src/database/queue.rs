use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use sqlx::{PgConnection, PgPool};

use crate::{
    model::{Capture, Source, Target},
    transport::validate_target,
};

use super::{Job, JobClass, keys};

#[tracing::instrument(skip(connection, targets))]
pub(super) async fn enqueue(
    connection: &mut PgConnection,
    targets: &[Target],
    refresh: bool,
) -> Result<u64> {
    let count = enqueue_rows(connection, targets, refresh).await?;
    if targets
        .iter()
        .any(|target| target.kind == crate::model::PageKind::Contacts)
    {
        note_contact_stops(connection).await?;
    }
    Ok(count)
}

#[tracing::instrument(skip(connection, targets))]
pub(super) async fn enqueue_rows(
    connection: &mut PgConnection,
    targets: &[Target],
    refresh: bool,
) -> Result<u64> {
    targets.iter().try_for_each(validate_target)?;
    let (_, count) = stream::iter(targets.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, 0_u64),
            |(connection, count), target| async move {
                let changed = enqueue_one(connection, target, refresh).await?;
                let total = count
                    .checked_add(changed)
                    .context("checkpoint count overflow")?;
                Ok((connection, total))
            },
        )
        .await?;
    Ok(count)
}

#[tracing::instrument(skip(connection))]
pub(super) async fn note_contact_stops(connection: &mut PgConnection) -> Result<()> {
    sqlx::query(
        "UPDATE scrape_pages p SET notes=array_append(p.notes, \
           'Contact acquisition stopped by blocked site checkpoint: ' || blocked.url || ' | ' || COALESCE(blocked.last_error,'access stop')) \
         FROM scrape_pages blocked \
         WHERE p.status IN ('pending','failed') AND p.cursor->>'kind'='contacts' \
         AND blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' \
         AND regexp_replace(lower(substring(blocked.url from '(?i)^https?://([^/?#]+)')), \
           CASE WHEN lower(blocked.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = \
         regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), \
           CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') \
         AND NOT ('Contact acquisition stopped by blocked site checkpoint: ' || blocked.url || ' | ' || COALESCE(blocked.last_error,'access stop')) = ANY(p.notes)",
    )
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection, target))]
async fn enqueue_one(connection: &mut PgConnection, target: &Target, refresh: bool) -> Result<u64> {
    Ok(sqlx::query(
        "INSERT INTO scrape_pages(page_key,source,url,cursor) \
         SELECT $1,$2,$3,$4 WHERE NOT ($6 AND EXISTS( \
           SELECT 1 FROM scrape_pages existing WHERE existing.source=$2 AND existing.url=$3 \
           AND existing.cursor->>'kind'='contacts' AND existing.cursor->'context'->>'school_id'=$7)) \
         ON CONFLICT(source,page_key) DO UPDATE SET status='pending',attempts=0,last_error=NULL,cursor=EXCLUDED.cursor \
         WHERE $5 AND scrape_pages.status = 'succeeded' AND scrape_pages.cursor->>'kind' <> 'contacts'",
    ).bind(keys::target(target)?).bind(target.source.as_str()).bind(&target.url)
        .bind(serde_json::to_value(target)?).bind(refresh)
        .bind(target.kind == crate::model::PageKind::Contacts).bind(&target.context.school_id)
        .execute(connection).await?.rows_affected())
}

fn attempt_limit(value: u32) -> Result<i32> {
    if !(1..=3).contains(&value) {
        bail!("maximum page attempts must be between 1 and 3");
    }
    i32::try_from(value).context("converting maximum attempts")
}

#[tracing::instrument(skip(pool, states))]
pub(super) async fn next(
    pool: &PgPool,
    source: Option<Source>,
    max_attempts: u32,
    preferred: JobClass,
    states: &[String],
) -> Result<Option<Job>> {
    let row: Option<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT p.id,p.cursor FROM scrape_pages p WHERE p.status IN ('pending','failed') AND p.attempts < $2 \
         AND ($1::text IS NULL OR p.source=$1) \
         AND (cardinality($4::text[]) = 0 OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($4) \
           OR (p.source='athleticnet' AND p.cursor->>'kind'='directory' \
             AND p.cursor->'context'->>'state' IS NULL \
             AND p.cursor->'context'->>'subject_id'=array_to_string($4,','))) \
         AND (p.cursor->>'kind' <> 'contacts' OR NOT EXISTS ( \
           SELECT 1 FROM scrape_pages blocked WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' \
           AND regexp_replace(lower(substring(blocked.url from '(?i)^https?://([^/?#]+)')), \
             CASE WHEN lower(blocked.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = \
           regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), \
             CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,''))) \
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

#[tracing::instrument(skip(pool))]
pub(super) async fn start_attempt(pool: &PgPool, page_id: i64, max_attempts: u32) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE scrape_pages p SET attempts=attempts+1,status='pending' \
         WHERE p.id=$1 AND p.status IN ('pending','failed') AND p.attempts < $2 \
         AND (p.cursor->>'kind' <> 'contacts' OR NOT EXISTS ( \
           SELECT 1 FROM scrape_pages blocked WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' \
           AND regexp_replace(lower(substring(blocked.url from '(?i)^https?://([^/?#]+)')), \
             CASE WHEN lower(blocked.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = \
           regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), \
             CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'')))",
    )
    .bind(page_id)
    .bind(attempt_limit(max_attempts)?)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

#[tracing::instrument(skip(connection, target))]
pub(super) async fn succeed(
    connection: &mut PgConnection,
    job_id: Option<i64>,
    target: &Target,
) -> Result<i64> {
    validate_target(target)?;
    let page_key = keys::target(target)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO scrape_pages(source,page_key,url,cursor,status,last_success) \
         VALUES($1,$2,$3,$4,'succeeded',clock_timestamp()) \
         ON CONFLICT(source,page_key) DO UPDATE SET status='succeeded',last_success=clock_timestamp(), \
         last_error=NULL,cursor=EXCLUDED.cursor RETURNING id",
    ).bind(target.source.as_str()).bind(page_key).bind(&target.url).bind(serde_json::to_value(target)?)
        .fetch_one(connection).await?;
    if job_id.is_some_and(|expected| expected != id) {
        bail!("page checkpoint ID does not match the supplied source request");
    }
    Ok(id)
}

#[tracing::instrument(skip(connection, job, response, detail))]
pub(super) async fn fail(
    connection: &mut PgConnection,
    job: &Job,
    response: Option<&Capture>,
    detail: &str,
    blocked: bool,
) -> Result<()> {
    let status = if blocked { "blocked" } else { "failed" };
    let error = response.map_or_else(
        || detail.to_owned(),
        |response| format!("HTTP {}: {detail}", response.status),
    );
    let id: Option<i64> = sqlx::query_scalar(
        "UPDATE scrape_pages SET status=$3,last_error=$4 WHERE id=$1 AND page_key=$2 RETURNING id",
    )
    .bind(job.id)
    .bind(keys::target(&job.target)?)
    .bind(status)
    .bind(error)
    .fetch_optional(&mut *connection)
    .await?;
    if id.is_none() {
        bail!(
            "page {} does not exist or does not match the supplied request",
            job.id
        );
    }
    if !blocked && response.is_some_and(|capture| capture.status == 404) {
        recover_contact_root(connection, job).await?;
    }
    if blocked && job.target.kind == crate::model::PageKind::Contacts {
        note_contact_stops(connection).await?;
    }
    Ok(())
}

#[tracing::instrument(skip(connection, job))]
async fn recover_contact_root(connection: &mut PgConnection, job: &Job) -> Result<()> {
    let Some(root) = crate::sources::contacts::recovery::after_not_found(&job.target)? else {
        return Ok(());
    };
    let alias_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scrape_pages WHERE source=$1 AND cursor->>'kind'='contacts' \
         AND cursor->'context'->>'school_id'=$2 AND url=$3)",
    )
    .bind(root.source.as_str())
    .bind(root.context.school_id.as_deref())
    .bind(root.url.trim_end_matches('/'))
    .fetch_one(&mut *connection)
    .await?;
    if !alias_exists {
        enqueue(connection, std::slice::from_ref(&root), false).await?;
    }
    let note = format!(
        "Published contact URL returned HTTP 404; same-site root checkpoint: {}",
        root.url
    );
    sqlx::query(
        "UPDATE scrape_pages SET notes=array_append(notes,$2) WHERE id=$1 AND NOT $2=ANY(notes)",
    )
    .bind(job.id)
    .bind(note)
    .execute(connection)
    .await?;
    Ok(())
}
