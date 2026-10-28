use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::{
    marks,
    model::{Performance, Source},
};

use super::{identity, keys, schools};

mod legacy_history;
mod raw_revisions;
mod school_evidence;

pub(super) use raw_revisions::review as review_revisions;
pub(super) struct Normalized {
    pub value: Option<i64>,
    pub unit: Option<&'static str>,
    pub specification: Option<String>,
    pub status: &'static str,
}

struct ResultRow<'a> {
    source: Source,
    performance: &'a Performance,
    location: (&'a str, usize),
    profile: i64,
    school: Option<i64>,
}

#[tracing::instrument(skip(connection, performances))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    source: Source,
    performances: &[Performance],
    url: &str,
) -> Result<()> {
    legacy_history::reconcile(connection, source, performances, url).await?;
    stream::iter(performances.iter().enumerate().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, (index, performance)| async move {
            save(connection, source, performance, (url, index)).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, performance))]
async fn save(
    connection: &mut PgConnection,
    source: Source,
    performance: &Performance,
    location: (&str, usize),
) -> Result<()> {
    if performance.wind.is_some_and(|wind| !wind.is_finite()) {
        bail!(
            "non-finite wind for {} athlete {}",
            source.as_str(),
            performance.athlete_id
        );
    }
    let profile = identity::find(connection, source, &performance.athlete_id).await?;
    let school = schools::find(connection, source, performance.school_id.as_deref()).await?;
    upsert(
        connection,
        ResultRow {
            source,
            performance,
            location,
            profile,
            school,
        },
    )
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection, row))]
async fn upsert(connection: &mut PgConnection, row: ResultRow<'_>) -> Result<i64> {
    let performance = row.performance;
    let reported_status = performance
        .reported_status
        .as_deref()
        .and_then(marks::failure_status);
    let key = keys::performance(row.source, performance, row.location.0, row.location.1)?;
    let normalized = normalize_effective(
        connection,
        &key,
        &performance.event,
        &performance.mark,
        performance.is_relay,
        reported_status,
    )
    .await?;
    let school_replay = school_evidence::resolve(connection, row.source, performance, &key).await?;
    sqlx::query_scalar(include_str!("result_upsert.sql"))
        .bind(key)
        .bind(row.profile)
        .bind(row.school)
        .bind(row.source.as_str())
        .bind(&performance.result_id)
        .bind(&performance.event)
        .bind(&performance.mark)
        .bind(performance.sport.as_str())
        .bind(&performance.meet_id)
        .bind(&performance.meet_name)
        .bind(&performance.date)
        .bind(performance.wind)
        .bind(&performance.timing)
        .bind(&performance.course)
        .bind(performance.is_personal_record)
        .bind(performance.is_relay)
        .bind(serde_json::to_value(performance)?)
        .bind(row.location.0)
        .bind(performance.season)
        .bind(&performance.grade)
        .bind(&performance.gender)
        .bind(normalized.value)
        .bind(normalized.unit)
        .bind(normalized.specification)
        .bind(normalized.status)
        .bind(school_replay.clear)
        .bind(school_replay.evidence)
        .fetch_optional(connection)
        .await?
        .context(
            "source result ID belongs to a different athlete; refusing to overwrite its identity",
        )
}

#[tracing::instrument(skip(connection))]
pub(super) async fn normalize_effective(
    connection: &mut PgConnection,
    key: &str,
    event: &str,
    mark: &str,
    relay: bool,
    reported_status: Option<&'static str>,
) -> Result<Normalized> {
    let previous: Option<(String, String, String)> =
        if event.trim().is_empty() || mark.trim().is_empty() {
            sqlx::query_as(
                "SELECT event,raw_mark,status FROM results WHERE source_result_key=$1 FOR UPDATE",
            )
            .bind(key)
            .fetch_optional(connection)
            .await?
        } else {
            None
        };
    let event = if event.trim().is_empty() {
        previous.as_ref().map_or(event, |row| row.0.as_str())
    } else {
        event
    };
    let effective_mark = if mark.trim().is_empty() {
        previous.as_ref().map_or(mark, |row| row.1.as_str())
    } else {
        mark
    };
    let failure = reported_status.or_else(|| {
        if mark.trim().is_empty() {
            previous
                .as_ref()
                .and_then(|row| marks::failure_status(&row.2))
        } else {
            None
        }
    });
    let (value, unit, specification) =
        normalization(event, effective_mark, relay || failure.is_some());
    Ok(Normalized {
        value,
        unit,
        specification,
        status: failure.map_or_else(|| status(effective_mark, value.is_some()), |status| status),
    })
}

pub(super) fn normalization(
    event: &str,
    mark: &str,
    relay: bool,
) -> (Option<i64>, Option<&'static str>, Option<String>) {
    let normalized = if relay {
        None
    } else {
        marks::normalized_mark(event, mark)
    };
    match normalized {
        Some((value, measure, specification)) => (Some(value), Some(measure.unit()), specification),
        None => (None, None, marks::event_specification(event)),
    }
}

pub(super) fn status(mark: &str, normalized: bool) -> &'static str {
    const STATUSES: [&str; 12] = [
        "DQ", "DNF", "DNS", "NH", "NM", "NT", "SCR", "FS", "FOUL", "DNQ", "WD", "RET",
    ];
    STATUSES
        .into_iter()
        .find(|status| mark.trim().eq_ignore_ascii_case(status))
        .map_or(
            if normalized {
                "recorded"
            } else {
                "unclassified"
            },
            |status| status,
        )
}
