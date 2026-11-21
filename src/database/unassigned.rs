use anyhow::{Result, ensure};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{Source, UnassignedResult};

use super::{keys, results, schools};

#[tracing::instrument(skip(connection, rows))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    source: Source,
    rows: &[UnassignedResult],
    url: &str,
) -> Result<()> {
    stream::iter(rows.iter().enumerate().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, (index, row)| async move {
            save(connection, source, row, (url, index)).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, row))]
async fn save(
    connection: &mut PgConnection,
    source: Source,
    row: &UnassignedResult,
    location: (&str, usize),
) -> Result<()> {
    let key = match row.result_id.as_deref().filter(|id| !id.trim().is_empty()) {
        Some(id) => keys::serialized(&(source, "result", id))?,
        None => match row.source_row.as_deref() {
            Some(source_row) => keys::serialized(&(source, "source_row", location.0, source_row))?,
            None => keys::serialized(&(source, "unassigned_page_row_content", location, row))?,
        },
    };
    let affected = upsert(
        connection,
        source,
        row,
        location,
        &key,
        row.result_id.as_deref(),
    )
    .await?;
    if affected == 0 {
        let rejected_key = keys::serialized(&(source, "rejected_result", location.0, row))?;
        ensure!(
            upsert(connection, source, row, location, &rejected_key, None).await? == 1,
            "rejected result evidence could not be retained"
        );
    }
    Ok(())
}

#[tracing::instrument(skip(connection, row))]
async fn upsert(
    connection: &mut PgConnection,
    source: Source,
    row: &UnassignedResult,
    location: (&str, usize),
    key: &str,
    result_id: Option<&str>,
) -> Result<u64> {
    let (saved, guarded) = write(connection, source, row, location, key, result_id, true).await?;
    if saved == 0 && guarded {
        return Ok(
            write(connection, source, row, location, key, result_id, false)
                .await?
                .0,
        );
    }
    Ok(saved)
}

#[tracing::instrument(skip(connection, row))]
async fn write(
    connection: &mut PgConnection,
    source: Source,
    row: &UnassignedResult,
    location: (&str, usize),
    key: &str,
    result_id: Option<&str>,
    guard_missing: bool,
) -> Result<(u64, bool)> {
    let school = schools::find(connection, source, row.school_id.as_deref()).await?;
    let reported_status = row
        .raw
        .get("statusCode")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::marks::failure_status);
    let (normalized, missing) = results::normalize_effective(
        connection,
        key,
        &row.event,
        &row.mark,
        row.is_relay,
        reported_status,
    )
    .await?;
    let guarded = guard_missing && missing;
    let saved = sqlx::query(include_str!("unassigned_upsert.sql"))
        .bind(key)
        .bind(school)
        .bind(source.as_str())
        .bind(result_id)
        .bind(&row.event)
        .bind(&row.mark)
        .bind(row.sport.as_str())
        .bind(&row.meet_id)
        .bind(&row.date)
        .bind(normalized.status)
        .bind(row.is_relay)
        .bind(location.0)
        .bind(serde_json::to_value(row)?)
        .bind(normalized.value)
        .bind(normalized.unit)
        .bind(normalized.specification)
        .bind(guarded)
        .execute(connection)
        .await?;
    Ok((saved.rows_affected(), guarded))
}
