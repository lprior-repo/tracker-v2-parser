use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::{
    marks,
    model::{Performance, Source},
};

use super::{identity, keys, schools};

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
    let (value, unit, specification) =
        normalization(&performance.event, &performance.mark, performance.is_relay);
    sqlx::query_scalar(include_str!("result_upsert.sql"))
        .bind(keys::performance(
            row.source,
            performance,
            row.location.0,
            row.location.1,
        )?)
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
        .bind(value)
        .bind(unit)
        .bind(specification)
        .bind(status(&performance.mark, value.is_some()))
        .fetch_optional(connection)
        .await?
        .context(
            "source result ID belongs to a different athlete; refusing to overwrite its identity",
        )
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
