use std::collections::BTreeMap;

use anyhow::Result;
use futures::{TryStreamExt, stream};
use serde_json::Value;
use sqlx::PgConnection;

use crate::model::{Capture, ParsedPage, Source, Target};

mod adoption;

pub(in crate::database) async fn prepare(
    connection: &mut PgConnection,
    target: &Target,
    capture: &Capture,
    page: &ParsedPage,
) -> Result<()> {
    if target.source == Source::MileSplit {
        super::legacy_history::review_masked(connection, target, capture).await?;
        adoption::reconcile(connection, page).await?;
    }
    Ok(())
}

#[tracing::instrument(skip(connection, page))]
pub(in crate::database) async fn review(
    connection: &mut PgConnection,
    source: Source,
    page: &ParsedPage,
) -> Result<Vec<String>> {
    if source != Source::MileSplit {
        return Ok(Vec::new());
    }
    super::legacy_history::review(connection, &page.performances).await?;
    stream::iter(observed(page).into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold((connection, Vec::new()), |(connection, mut notes), (file, ids)| async move {
            let missing = mark_revision(connection, file, &ids).await?;
            if missing > 0 {
                notes.push(format!("Raw published file {file}: {missing} prior observations were not reconfirmed in this response; retained for review and excluded from PR winners. Response absence does not establish deletion or cross-event correction."));
            }
            Ok((connection, notes))
        }).await.map(|(_, notes)| notes)
}

fn observed(page: &ParsedPage) -> BTreeMap<&str, Vec<&str>> {
    page.performances
        .iter()
        .filter_map(|row| Some((row.source_values.as_ref()?, row.result_id.as_deref()?)))
        .chain(
            page.unassigned_results
                .iter()
                .filter_map(|row| Some((&row.raw, row.result_id.as_deref()?))),
        )
        .filter_map(|(values, id)| proven_file(values).map(|file| (file, id)))
        .fold(BTreeMap::new(), |mut observed, (file, id)| {
            observed.entry(file).or_insert_with(Vec::new).push(id);
            observed
        })
}

fn proven_file(values: &Value) -> Option<&str> {
    if values.get("kind")?.as_str()? != "raw_result" {
        return None;
    }
    let file = values.get("published_file_identity")?.as_str()?;
    let (meet, result_set) = file.strip_prefix("milesplit-file:")?.split_once(':')?;
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    (numeric(meet) && numeric(result_set)).then_some(file)
}

#[tracing::instrument(skip(connection, ids))]
async fn mark_revision(connection: &mut PgConnection, file: &str, ids: &[&str]) -> Result<i64> {
    let (missing, _, _): (i64, i64, i64) = sqlx::query_as(
        "WITH reconfirmed AS ( \
         UPDATE results SET raw=raw #- '{source_values,revision_status}' #- '{raw,revision_status}' \
         WHERE source='milesplit' AND source_result_id=ANY($2) \
         AND ((raw#>>'{source_values,kind}'='raw_result' AND raw#>>'{source_values,published_file_identity}'=$1 \
               AND raw#>>'{source_values,revision_status}'='unverified_prior_revision') \
           OR (raw#>>'{raw,kind}'='raw_result' AND raw#>>'{raw,published_file_identity}'=$1 \
               AND raw#>>'{raw,revision_status}'='unverified_prior_revision')) RETURNING id \
         ), missing AS MATERIALIZED ( \
         SELECT id,athlete_id FROM results WHERE source='milesplit' AND NOT(source_result_id=ANY($2)) \
         AND ((raw#>>'{source_values,kind}'='raw_result' AND raw#>>'{source_values,published_file_identity}'=$1) \
           OR (raw#>>'{raw,kind}'='raw_result' AND raw#>>'{raw,published_file_identity}'=$1)) \
         ), revised AS ( \
         UPDATE results r SET raw=CASE WHEN r.raw#>>'{source_values,kind}'='raw_result' \
           THEN jsonb_set(r.raw,'{source_values,revision_status}','\"unverified_prior_revision\"'::jsonb,TRUE) \
           ELSE jsonb_set(r.raw,'{raw,revision_status}','\"unverified_prior_revision\"'::jsonb,TRUE) END \
         FROM missing m WHERE r.id=m.id \
         AND COALESCE(r.raw#>>'{source_values,revision_status}',r.raw#>>'{raw,revision_status}','')<>'unverified_prior_revision' \
         RETURNING r.id \
         ), reviewed AS ( \
         UPDATE athletes SET needs_review=TRUE WHERE NOT needs_review AND id IN (SELECT athlete_id FROM missing) RETURNING id \
         ) SELECT (SELECT count(*) FROM missing),(SELECT count(*) FROM revised),(SELECT count(*) FROM reconfirmed)",
    ).bind(file).bind(ids).fetch_one(connection).await?;
    Ok(missing)
}
