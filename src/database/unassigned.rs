use anyhow::Result;
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
    let school = schools::find(connection, source, row.school_id.as_deref()).await?;
    let key = match row.result_id.as_deref().filter(|id| !id.trim().is_empty()) {
        Some(id) => keys::serialized(&(source, "result", id))?,
        None => match row.source_row.as_deref() {
            Some(source_row) => keys::serialized(&(source, "source_row", location.0, source_row))?,
            None => keys::serialized(&(source, "unassigned_page_row_content", location, row))?,
        },
    };
    let (value, unit, specification) = results::normalization(&row.event, &row.mark, row.is_relay);
    sqlx::query(include_str!("unassigned_upsert.sql"))
        .bind(key)
        .bind(school)
        .bind(source.as_str())
        .bind(&row.result_id)
        .bind(&row.event)
        .bind(&row.mark)
        .bind(row.sport.as_str())
        .bind(&row.meet_id)
        .bind(&row.date)
        .bind(results::status(&row.mark, value.is_some()))
        .bind(row.is_relay)
        .bind(location.0)
        .bind(serde_json::to_value(row)?)
        .bind(value)
        .bind(unit)
        .bind(specification)
        .execute(connection)
        .await?;
    Ok(())
}
