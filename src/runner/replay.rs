use std::{borrow::Cow, path::Path};

use anyhow::{Context, Result, anyhow, ensure};
use tokio::io::AsyncReadExt;

use crate::{
    config::Config,
    database::{Database, Job},
    model::{Capture, Source, Target},
    sources,
    transport::{MAX_BODY_BYTES, validate_target},
};

async fn persist_capture(
    database: &Database,
    job: &Job,
    capture: Capture,
    evidence_root: &Path,
) -> Result<Option<anyhow::Error>> {
    let (capture, retained) = match retain_profile_capture(job, capture, evidence_root).await {
        Ok(retained) => retained,
        Err(error) => {
            return record_failure(
                database,
                job,
                None,
                false,
                error.context("retaining profile evidence"),
            )
            .await
            .map(|()| None);
        }
    };
    let parse_target =
        if job.target.kind == crate::model::PageKind::Contacts && job.target.url != capture.url {
            let mut target = job.target.clone();
            target.url.clone_from(&capture.url);
            Cow::Owned(target)
        } else {
            Cow::Borrowed(&job.target)
        };
    let parsed = match std::str::from_utf8(&capture.body)
        .context("source response is not UTF-8")
        .and_then(|body| sources::parse(&parse_target, body))
    {
        Ok(mut parsed) => {
            if let Some(evidence) = retained {
                evidence.attach(&mut parsed, &job.target.url)?;
            }
            parsed
        }
        Err(error) => {
            let blocked = error.is::<sources::AccessDenied>()
                || error.is::<sources::contacts::AccessDenied>();
            let error = error.context("parsing source page");
            let error = match retained {
                Some(evidence) => error.context(evidence.describe()),
                None => error,
            };
            let detail = format!("{error:#}");
            database
                .fail_job(job, Some(&capture), &detail, blocked)
                .await
                .with_context(|| format!("recording page failure after: {detail}"))?;
            if blocked && job.target.kind != crate::model::PageKind::Contacts {
                return Err(error);
            }
            return Ok(Some(error));
        }
    };
    match database
        .persist(Some(job.id), &job.target, &capture, &parsed)
        .await
    {
        Ok(_) => Ok(None),
        Err(error) => record_failure(
            database,
            job,
            Some(&capture),
            false,
            error.context("persisting source page"),
        )
        .await
        .map(|()| None),
    }
}

async fn retain_profile_capture(
    job: &Job,
    capture: Capture,
    evidence_root: &Path,
) -> Result<(Capture, Option<crate::profile_evidence::RetainedCapture>)> {
    if job.target.source != Source::MileSplit || job.target.kind != crate::model::PageKind::Athlete
    {
        return Ok((capture, None));
    }
    crate::profile_evidence::retain_at(evidence_root, capture)
        .await
        .map(|(capture, evidence)| (capture, Some(evidence)))
}

async fn record_failure(
    database: &Database,
    job: &Job,
    capture: Option<&Capture>,
    blocked: bool,
    error: anyhow::Error,
) -> Result<()> {
    let detail = format!("{error:#}");
    database
        .fail_job(job, capture, &detail, blocked)
        .await
        .with_context(|| format!("recording page failure after: {detail}"))?;
    Err(error)
}

pub(super) async fn replay_page(
    database: &Database,
    config: &Config,
    input: &Path,
    target: Target,
    evidence_root: &Path,
) -> Result<()> {
    validate_target(&target)?;
    database
        .enqueue(std::slice::from_ref(&target), true)
        .await?;
    let cursor = serde_json::to_value(&target)?;
    let (id, cursor): (i64, serde_json::Value) = sqlx::query_as(
        "SELECT id,cursor FROM scrape_pages WHERE source=$1 AND (cursor=$2 OR ( \
         cursor->>'kind'='contacts' AND $2::jsonb->>'kind'='contacts' \
         AND url=$2::jsonb->>'url' \
         AND cursor->'context'->>'school_id'=$2::jsonb->'context'->>'school_id')) \
         ORDER BY (cursor=$2) DESC,id LIMIT 1",
    )
    .bind(target.source.as_str())
    .bind(cursor)
    .fetch_one(database.pool())
    .await
    .context("finding replay checkpoint")?;
    let job = Job {
        id,
        target: serde_json::from_value(cursor).context("decoding replay checkpoint target")?,
    };
    if !database
        .start_attempt(id, config.limits.max_attempts)
        .await?
    {
        let replayable: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM scrape_pages p WHERE p.id=$1 \
             AND (p.status='succeeded' OR (p.status IN ('pending','failed') AND p.attempts >= $2)) \
             AND (p.cursor->>'kind' <> 'contacts' OR NOT EXISTS ( \
               SELECT 1 FROM scrape_pages blocked CROSS JOIN LATERAL unnest(ARRAY[blocked.url,blocked.cursor->>'access_stop_url']) AS stop(url) WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' \
               AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')), \
                 CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = \
               regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), \
                 CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,''))))",
        )
        .bind(id)
        .bind(i32::try_from(config.limits.max_attempts).context("converting maximum attempts")?)
        .fetch_one(database.pool())
        .await
        .context("checking replay checkpoint")?;
        ensure!(replayable, "replay checkpoint could not start an attempt");
    }
    let body = match read_replay(input).await {
        Ok(body) => body,
        Err(error) => return record_failure(database, &job, None, false, error).await,
    };
    let capture = match crate::transport::prepare_response(
        job.target.kind,
        Capture {
            url: job.target.url.clone(),
            status: 200,
            content_type: String::from("application/octet-stream"),
            body,
        },
    ) {
        Ok(capture) => capture,
        Err(failure) => {
            return record_failure(
                database,
                &job,
                failure.capture.as_ref(),
                failure.blocked,
                anyhow!(failure.detail),
            )
            .await;
        }
    };
    persist_capture(database, &job, capture, evidence_root)
        .await?
        .map_or_else(|| Ok(()), Err)
}

async fn read_replay(input: &Path) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(input)
        .await
        .with_context(|| format!("opening replay input {}", input.display()))?;
    let limit = u64::try_from(MAX_BODY_BYTES)
        .context("converting replay size limit")?
        .saturating_add(1);
    let mut body = Vec::new();
    file.take(limit)
        .read_to_end(&mut body)
        .await
        .context("reading replay input")?;
    ensure!(
        body.len() <= MAX_BODY_BYTES,
        "replay input exceeds the response size limit"
    );
    Ok(body)
}
