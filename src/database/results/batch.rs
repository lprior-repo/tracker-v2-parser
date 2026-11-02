use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, QueryBuilder};

use crate::model::{Performance, Source};

use super::super::keys;
use super::{
    Normalized, normalization,
    references::{self, References},
};
use serialization::{bind_payload_bytes, serialize_raw};

mod serialization;

const MIN_ROWS: usize = 8;
const MAX_BYTES: usize = 1024 * 1024;
const CROSS_ATHLETE_ERROR: &str =
    "source result ID belongs to a different athlete; refusing to overwrite its identity";

pub(super) enum BatchOutcome {
    Saved,
    Fallback,
}

struct PreparedRow<'a> {
    key: String,
    performance: &'a Performance,
    profile: i64,
    school: Option<i64>,
    raw: Value,
    normalized: Normalized,
}

#[tracing::instrument(skip(connection, rows, references))]
pub(super) async fn try_save(
    connection: &mut PgConnection,
    source: Source,
    rows: &[Performance],
    url: &str,
    offset: usize,
    references: &References,
) -> Result<BatchOutcome> {
    if rows.len() < MIN_ROWS || rows.len() > references::ROWS {
        return Ok(BatchOutcome::Fallback);
    }
    let prepared = match prepare(source, rows, url, offset, references) {
        Preparation::Ready(prepared) => prepared,
        Preparation::Fallback => return Ok(BatchOutcome::Fallback),
    };
    let Ok(sql) = batch_sql() else {
        return Ok(BatchOutcome::Fallback);
    };
    persist(connection, source, &prepared, url, sql).await?;
    Ok(BatchOutcome::Saved)
}

enum Preparation<'a> {
    Ready(Vec<PreparedRow<'a>>),
    Fallback,
}

fn prepare<'a>(
    source: Source,
    rows: &'a [Performance],
    url: &str,
    offset: usize,
    references: &References,
) -> Preparation<'a> {
    if rows.is_empty() || rows.len() > references::ROWS {
        return Preparation::Fallback;
    }
    let prepared = rows.iter().enumerate().try_fold(
        (BTreeSet::new(), 0_usize, Vec::with_capacity(rows.len())),
        |(mut keys_seen, bytes, mut prepared), (index, performance)| {
            if !eligible(source, performance, references) {
                return Err(());
            }
            let row_index = offset.checked_add(index).ok_or(())?;
            let key = keys::performance(source, performance, url, row_index).map_err(|_| ())?;
            if !keys_seen.insert(key.clone()) {
                return Err(());
            }
            let profile = references
                .cached_profile(&performance.athlete_id)
                .ok_or(())?;
            let school = match performance.school_id.as_deref() {
                Some(id) => Some(references.cached_school(id).ok_or(())?),
                None => None,
            };
            let normalized = normalized(performance);
            let bind_bytes =
                bind_payload_bytes(source, performance, &key, url, &normalized).ok_or(())?;
            let raw_limit = MAX_BYTES
                .checked_sub(bytes)
                .and_then(|left| left.checked_sub(bind_bytes))
                .ok_or(())?;
            let (raw, json_size) = serialize_raw(performance, raw_limit).ok_or(())?;
            let row_bytes = bind_bytes.checked_add(json_size).ok_or(())?;
            let bytes = checked_payload_bytes(bytes, row_bytes).ok_or(())?;
            prepared.push(PreparedRow {
                key,
                performance,
                profile,
                school,
                raw,
                normalized,
            });
            Ok((keys_seen, bytes, prepared))
        },
    );
    match prepared {
        Ok((_, _, prepared)) => Preparation::Ready(prepared),
        Err(()) => Preparation::Fallback,
    }
}

fn eligible(source: Source, performance: &Performance, references: &References) -> bool {
    performance.wind.is_none_or(f64::is_finite)
        && !performance.event.trim().is_empty()
        && !performance.mark.trim().is_empty()
        && references.has_profile(&performance.athlete_id)
        && references.has_school(performance.school_id.as_deref())
        && !school_replay_candidate(source, performance)
}

fn school_replay_candidate(source: Source, performance: &Performance) -> bool {
    source == Source::MileSplit
        && performance.school_id.is_none()
        && performance
            .source_values
            .as_ref()
            .and_then(|values| values.get("school_attribution"))
            .and_then(Value::as_str)
            == Some("not_published")
}

fn normalized(performance: &Performance) -> super::Normalized {
    let reported_status = performance
        .reported_status
        .as_deref()
        .and_then(crate::marks::failure_status);
    let (value, unit, specification) = normalization(
        &performance.event,
        &performance.mark,
        performance.is_relay || reported_status.is_some(),
    );
    super::Normalized {
        value,
        unit,
        specification,
        status: reported_status.map_or_else(
            || super::status(&performance.mark, value.is_some()),
            |status| status,
        ),
    }
}

fn checked_payload_bytes(current: usize, row: usize) -> Option<usize> {
    current.checked_add(row).filter(|total| *total <= MAX_BYTES)
}

async fn persist(
    connection: &mut PgConnection,
    source: Source,
    rows: &[PreparedRow<'_>],
    url: &str,
    sql: (String, String),
) -> Result<()> {
    let mut query = QueryBuilder::<Postgres>::new(sql.0);
    query.push_values(rows, |mut bind, row| {
        let performance = row.performance;
        bind.push_bind(&row.key)
            .push_bind(row.profile)
            .push_unseparated(", (SELECT athlete_id FROM athlete_sources WHERE id = ")
            .push_bind_unseparated(row.profile)
            .push_unseparated(")")
            .push_bind(row.school)
            .push_bind(source.as_str())
            .push_bind(performance.result_id.as_deref())
            .push_bind(&performance.event)
            .push_bind(&performance.mark)
            .push_bind(performance.sport.as_str())
            .push_bind(performance.meet_id.as_deref())
            .push_bind(performance.meet_name.as_deref())
            .push_bind(performance.date.as_deref())
            .push_bind(performance.wind)
            .push_bind(performance.timing.as_deref())
            .push_bind(performance.course.as_deref())
            .push_bind(performance.is_personal_record)
            .push_bind(performance.is_relay)
            .push_bind(sqlx::types::Json(&row.raw))
            .push_bind(url)
            .push_bind(performance.season)
            .push_bind(performance.grade.as_deref())
            .push_bind(performance.gender.as_deref())
            .push_bind(row.normalized.value)
            .push_bind(row.normalized.unit)
            .push_bind(&row.normalized.specification)
            .push_bind(row.normalized.status);
    });
    query.push(" ").push(sql.1);
    let returned = query
        .build_query_scalar::<String>()
        .fetch_all(connection)
        .await?;
    let returned = returned.into_iter().collect::<BTreeSet<_>>();
    let missing = rows.iter().find(|row| !returned.contains(&row.key));
    if missing.is_some() {
        bail!(CROSS_ATHLETE_ERROR);
    }
    Ok(())
}

fn batch_sql() -> Result<(String, String)> {
    let statement = include_str!("../result_upsert.sql");
    let (prefix, _) = statement
        .split_once("VALUES (")
        .context("result upsert values clause changed")?;
    let (_, conflict) = statement
        .split_once(")\nON CONFLICT (source_result_key) DO UPDATE SET")
        .context("result upsert conflict clause changed")?;
    let conflict = format!("ON CONFLICT (source_result_key) DO UPDATE SET{conflict}")
        .replace("$26", "FALSE")
        .replace("$27::jsonb", "NULL::jsonb")
        .replace("RETURNING id", "RETURNING source_result_key");
    if conflict.contains("$26") || conflict.contains("$27") {
        bail!("result upsert replay controls could not be specialized");
    }
    if !conflict.ends_with("RETURNING source_result_key\n") {
        bail!("result upsert returning clause changed");
    }
    let sql = prefix.to_owned();
    Ok((sql, conflict))
}

#[cfg(test)]
#[path = "batch/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "batch/differential_tests.rs"]
mod differential_tests;

#[cfg(test)]
#[path = "batch/measurement_tests.rs"]
mod measurement_tests;
