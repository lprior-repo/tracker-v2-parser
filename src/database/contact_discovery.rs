use anyhow::{Context, Result};
use futures::{TryStreamExt, stream};
use sqlx::{PgConnection, PgPool};

use crate::{
    model::{School, Source},
    sources,
};

use super::queue;

#[tracing::instrument(skip(pool, states, sources))]
pub(super) async fn backfill(
    pool: &PgPool,
    states: &[String],
    sources: &[Source],
    season: i32,
) -> Result<u64> {
    let enabled: Vec<_> = sources.iter().copied().map(Source::as_str).collect();
    let mut transaction = pool.begin().await?;
    queue::lock_contacts(&mut transaction).await?;
    let count = stream_schools(&mut transaction, &enabled, states, season)
        .await
        .context("backfilling official school contact websites")?;
    queue::note_contact_stops(&mut transaction).await?;
    transaction
        .commit()
        .await
        .context("committing contact website backfill")?;
    Ok(count)
}

#[tracing::instrument(skip_all)]
async fn stream_schools(
    connection: &mut PgConnection,
    sources: &[&str],
    states: &[String],
    season: i32,
) -> Result<u64> {
    sqlx::query(
        "DECLARE contact_backfill_schools NO SCROLL CURSOR FOR \
         SELECT source,jsonb_build_object('id',source_id,'name',name,'state',state,'website',website) \
         FROM schools WHERE source=ANY($1) AND state=ANY($2) \
         AND website IS NOT NULL AND btrim(website) <> '' ORDER BY id",
    )
    .bind(sources)
    .bind(states)
    .execute(&mut *connection)
    .await?;
    stream::try_unfold(connection, |connection| async move {
        let rows = sqlx::query_as::<_, (String, serde_json::Value)>(
            "FETCH FORWARD 128 FROM contact_backfill_schools",
        )
        .fetch_all(&mut *connection)
        .await?;
        if rows.is_empty() {
            return Ok::<_, anyhow::Error>(None);
        }
        let (connection, count) = stream::iter(rows.into_iter().map(Ok))
            .try_fold((connection, 0_u64), |(connection, count), row| async move {
                let changed = enqueue_school(connection, row, season).await?;
                Ok::<_, anyhow::Error>((connection, add_count(count, changed)?))
            })
            .await?;
        Ok(Some((count, connection)))
    })
    .try_fold(0_u64, |count, changed| {
        futures::future::ready(add_count(count, changed))
    })
    .await
}

#[tracing::instrument(skip_all)]
async fn enqueue_school(
    connection: &mut PgConnection,
    (source, school): (String, serde_json::Value),
    season: i32,
) -> Result<u64> {
    let source: Source = serde_json::from_value(serde_json::Value::String(source))?;
    let school: School = serde_json::from_value(school)?;
    let targets = match sources::school_contacts(source, &school, season) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(school_id = %school.id, "contact discovery skipped: {error:#}");
            return Ok(0);
        }
    };
    queue::enqueue_rows_locked(connection, &targets, false).await
}

fn add_count(count: u64, changed: u64) -> Result<u64> {
    count
        .checked_add(changed)
        .context("contact checkpoint count overflow")
}
