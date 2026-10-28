use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::Value;
use sqlx::PgConnection;
use url::Url;

use crate::model::{ParsedPage, Source};

struct Observation<'a> {
    id: &'a str,
    evidence: &'a Value,
    athlete: Option<&'a str>,
    file: String,
}

fn coordinates(id: &str) -> Option<(&str, Vec<Value>)> {
    let (kind, encoded) = id.split_once(':')?;
    let values: Vec<Value> = serde_json::from_str(encoded).ok()?;
    let expected = match kind {
        "raw" => 6,
        "table" => 5,
        _ => return None,
    };
    (values.len() == expected).then_some((kind, values))
}

fn signature(kind: &str, values: &[Value]) -> Result<String> {
    Ok(serde_json::to_string(&(
        kind,
        values.first(),
        values.get(2..),
    ))?)
}

fn observed(page: &ParsedPage) -> Result<BTreeMap<String, Vec<Observation<'_>>>> {
    page.performances
        .iter()
        .filter_map(|row| {
            Some((
                row.result_id.as_deref()?,
                row.source_values.as_ref()?,
                Some(row.athlete_id.as_str()),
            ))
        })
        .chain(
            page.unassigned_results
                .iter()
                .filter_map(|row| Some((row.result_id.as_deref()?, &row.raw, None))),
        )
        .try_fold(
            BTreeMap::<String, Vec<Observation<'_>>>::new(),
            |mut observed, (id, evidence, athlete)| {
                if let Some((kind, values)) = coordinates(id)
                    && let Some(file) = values.get(1).and_then(Value::as_str)
                    && file.strip_prefix("milesplit-file:").is_some()
                {
                    observed
                        .entry(signature(kind, &values)?)
                        .or_default()
                        .push(Observation {
                            id,
                            evidence,
                            athlete,
                            file: file.to_owned(),
                        });
                }
                Ok(observed)
            },
        )
}

pub(super) async fn reconcile(connection: &mut PgConnection, page: &ParsedPage) -> Result<()> {
    let incoming = observed(page)?;
    if incoming.is_empty() {
        return Ok(());
    }
    let files = incoming
        .values()
        .flatten()
        .map(|row| row.file.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let meets = page
        .performances
        .iter()
        .filter_map(|row| row.meet_id.as_deref())
        .chain(
            page.unassigned_results
                .iter()
                .filter_map(|row| row.meet_id.as_deref()),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let rows = sqlx::query_as::<_, (i64, String, String, Value)>(
        "SELECT id,source_result_id,source_url,raw FROM results WHERE source='milesplit' \
         AND meet_id=ANY($1) AND (source_result_id LIKE 'raw:%' OR source_result_id LIKE 'table:%') \
         AND NOT COALESCE((raw#>>'{source_values,kind}'='raw_result' \
           AND raw#>>'{source_values,published_file_identity}' ~ '^milesplit-file:[0-9]+:[0-9]+$') \
           OR (raw#>>'{raw,kind}'='raw_result' \
           AND raw#>>'{raw,published_file_identity}' ~ '^milesplit-file:[0-9]+:[0-9]+$'),FALSE) \
         ORDER BY id LIMIT 10001 FOR UPDATE",
    ).bind(meets).fetch_all(&mut *connection).await?;
    ensure!(
        rows.len() <= 10000,
        "legacy replay reconciliation exceeds 10000 observations"
    );
    stream::iter(rows.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, (row, id, url, raw)| {
            let incoming = &incoming;
            let files = &files;
            async move {
                let Some((kind, values)) = coordinates(&id) else {
                    return Ok(connection);
                };
                if values
                    .get(1)
                    .and_then(Value::as_str)
                    .is_some_and(|file| file.starts_with("milesplit-file:"))
                {
                    return Ok(connection);
                }
                if file_identity(&url).is_some_and(|file| !files.contains(file.as_str())) {
                    return Ok(connection);
                }
                let Some(candidates) = incoming.get(&signature(kind, &values)?) else {
                    if file_identity(&url).is_some_and(|file| files.contains(file.as_str())) {
                        quarantine(connection, row).await?;
                    }
                    return Ok(connection);
                };
                match candidates.as_slice() {
                    [current] if equivalent(current, kind, &values, &url, &raw) => {
                        adopt(connection, row, current.id).await?;
                    }
                    _ => quarantine(connection, row).await?,
                }
                Ok(connection)
            }
        })
        .await
        .map(|_| ())
}

fn equivalent(
    current: &Observation<'_>,
    kind: &str,
    coordinates: &[Value],
    location: &str,
    raw: &Value,
) -> bool {
    let Some(evidence) = raw.get("source_values").or_else(|| raw.get("raw")) else {
        return false;
    };
    let source = evidence.get("source_url").and_then(Value::as_str);
    let retained_html = evidence.get("published_html").and_then(Value::as_str);
    let old_file = coordinates.get(1).and_then(Value::as_str);
    let bound = match kind {
        "raw" => current.file.rsplit(':').next() == old_file,
        "table" => old_file.and_then(file_identity).as_deref() == Some(current.file.as_str()),
        _ => false,
    };
    bound
        && current.athlete == raw.get("athlete_id").and_then(Value::as_str)
        && file_identity(location).as_deref() == Some(current.file.as_str())
        && source.and_then(file_identity).as_deref() == Some(current.file.as_str())
        && retained_html.is_some_and(|html| !html.is_empty())
        && evidence.get("published_html") == current.evidence.get("published_html")
        && evidence.get("published_text") == current.evidence.get("published_text")
        && evidence.get("event") == current.evidence.get("event")
        && evidence.get("round") == current.evidence.get("round")
        && evidence.get("line_ordinal") == current.evidence.get("line_ordinal")
}

fn file_identity(location: &str) -> Option<String> {
    let url = Url::parse(location).ok()?;
    let host = url.host_str()?;
    if !matches!(url.scheme(), "https" | "http")
        || !(host == "milesplit.com" || host.ends_with(".milesplit.com"))
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let parts = url
        .path()
        .trim_end_matches('/')
        .strip_prefix('/')?
        .split('/')
        .collect::<Vec<_>>();
    let (["meets", meet, "results", file] | ["meets", meet, "results", file, "raw" | "formatted"]) =
        parts.as_slice()
    else {
        return None;
    };
    let meet = meet.split('-').next()?;
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    (numeric(meet) && numeric(file)).then(|| format!("milesplit-file:{meet}:{file}"))
}

async fn adopt(connection: &mut PgConnection, row: i64, id: &str) -> Result<()> {
    let key = crate::database::keys::serialized(&(Source::MileSplit, "result", id))?;
    let updated = sqlx::query(
        "UPDATE results SET source_result_key=$2,source_result_id=$3, \
         raw=jsonb_set(raw,'{result_id}',to_jsonb($3::text)) \
         WHERE id=$1 AND NOT EXISTS(SELECT 1 FROM results WHERE source_result_key=$2 AND id<>$1)",
    )
    .bind(row)
    .bind(key)
    .bind(id)
    .execute(&mut *connection)
    .await?;
    if updated.rows_affected() != 1 {
        quarantine(connection, row).await?;
    }
    Ok(())
}

async fn quarantine(connection: &mut PgConnection, row: i64) -> Result<()> {
    sqlx::query(
        "UPDATE results SET raw=jsonb_set(raw, \
         ARRAY[CASE WHEN athlete_source_id IS NOT NULL THEN 'source_values' ELSE 'raw' END], \
         (CASE WHEN athlete_source_id IS NOT NULL AND jsonb_typeof(raw->'source_values')='object' \
            THEN raw->'source_values' WHEN athlete_source_id IS NULL AND jsonb_typeof(raw->'raw')='object' \
            THEN raw->'raw' ELSE '{}'::jsonb END) \
         || jsonb_build_object('revision_status','unverified_prior_revision'),TRUE) WHERE id=$1",
    ).bind(row).execute(&mut *connection).await?;
    sqlx::query("UPDATE athletes SET needs_review=TRUE WHERE id=(SELECT athlete_id FROM results WHERE id=$1)")
        .bind(row).execute(connection).await?;
    Ok(())
}
