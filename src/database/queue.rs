use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use sqlx::{PgConnection, PgPool};

use crate::{
    model::{Capture, Source, Target},
    transport::validate_target,
};

use super::{Job, keys};

#[tracing::instrument(skip(connection, targets))]
pub(super) async fn enqueue(
    connection: &mut PgConnection,
    targets: &[Target],
    refresh: bool,
) -> Result<u64> {
    targets.iter().try_for_each(validate_target)?;
    stream::iter(targets.iter().map(Ok::<_, anyhow::Error>))
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
        .await
        .map(|(_, count)| count)
}

#[tracing::instrument(skip(connection, target))]
async fn enqueue_one(connection: &mut PgConnection, target: &Target, refresh: bool) -> Result<u64> {
    Ok(sqlx::query(
        "INSERT INTO scrape_pages(page_key,source,url,cursor) VALUES($1,$2,$3,$4) \
         ON CONFLICT(source,page_key) DO UPDATE SET status='pending',attempts=0,last_error=NULL,cursor=EXCLUDED.cursor \
         WHERE $5 AND scrape_pages.status <> 'blocked'",
    ).bind(keys::target(target)?).bind(target.source.as_str()).bind(&target.url)
        .bind(serde_json::to_value(target)?).bind(refresh)
        .execute(connection).await?.rows_affected())
}

fn attempt_limit(value: u32) -> Result<i32> {
    if !(1..=3).contains(&value) {
        bail!("maximum page attempts must be between 1 and 3");
    }
    i32::try_from(value).context("converting maximum attempts")
}

#[tracing::instrument(skip(pool))]
pub(super) async fn next(
    pool: &PgPool,
    source: Option<Source>,
    max_attempts: u32,
) -> Result<Option<Job>> {
    let row: Option<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT id,cursor FROM scrape_pages WHERE status IN ('pending','failed') AND attempts < $2 \
         AND ($1::text IS NULL OR source=$1) ORDER BY id LIMIT 1",
    )
    .bind(source.map(Source::as_str))
    .bind(attempt_limit(max_attempts)?)
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
        "UPDATE scrape_pages SET attempts=attempts+1,status='pending' \
         WHERE id=$1 AND status IN ('pending','failed') AND attempts < $2",
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
    .fetch_optional(connection)
    .await?;
    if id.is_none() {
        bail!(
            "page {} does not exist or does not match the supplied request",
            job.id
        );
    }
    Ok(())
}
