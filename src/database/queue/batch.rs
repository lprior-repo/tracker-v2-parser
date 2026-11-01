use anyhow::{Context, Result};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::Target;

use super::super::keys;

const ROWS: usize = 128;
const BYTES: usize = 1024 * 1024;

struct Row<'a> {
    target: &'a Target,
    key: String,
    cursor: String,
    bytes: usize,
}

impl<'a> Row<'a> {
    fn prepare(target: &'a Target) -> Result<Self> {
        let key = keys::target(target)?;
        let cursor = serde_json::to_string(target)?;
        let bytes = key
            .len()
            .checked_add(cursor.len())
            .and_then(|size| size.checked_add(target.url.len()))
            .and_then(|size| size.checked_add(target.source.as_str().len()))
            .context("queued target byte count overflow")?;
        Ok(Self {
            target,
            key,
            cursor,
            bytes,
        })
    }
}

#[derive(Default)]
struct Pending<'a> {
    rows: Vec<Row<'a>>,
    bytes: usize,
}

impl<'a> Pending<'a> {
    fn full(&self, row: &Row<'_>) -> Result<bool> {
        Ok(self.rows.len() >= ROWS
            || self
                .bytes
                .checked_add(row.bytes)
                .context("queue batch byte count overflow")?
                > BYTES
            || self.rows.iter().any(|existing| {
                existing.key == row.key && existing.target.source == row.target.source
            }))
    }

    fn push(&mut self, row: Row<'a>) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(row.bytes)
            .context("queue batch byte count overflow")?;
        self.rows
            .try_reserve(1)
            .context("allocating bounded queue batch")?;
        self.rows.push(row);
        Ok(())
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn enqueue(connection: &mut PgConnection, targets: &[Target]) -> Result<u64> {
    let (connection, pending, count) = stream::iter(targets.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, Pending::default(), 0_u64),
            |(connection, mut pending, count), target| async move {
                let row = Row::prepare(target)?;
                let count = if pending.full(&row)? {
                    let count = count
                        .checked_add(write(connection, &pending.rows).await?)
                        .context("checkpoint count overflow")?;
                    pending = Pending::default();
                    count
                } else {
                    count
                };
                pending.push(row)?;
                Ok((connection, pending, count))
            },
        )
        .await?;
    count
        .checked_add(write(connection, &pending.rows).await?)
        .context("checkpoint count overflow")
}

#[tracing::instrument(skip_all)]
async fn write(connection: &mut PgConnection, rows: &[Row<'_>]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let keys = rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>();
    let sources = rows
        .iter()
        .map(|row| row.target.source.as_str())
        .collect::<Vec<_>>();
    let urls = rows
        .iter()
        .map(|row| row.target.url.as_str())
        .collect::<Vec<_>>();
    let cursors = rows
        .iter()
        .map(|row| row.cursor.as_str())
        .collect::<Vec<_>>();
    Ok(sqlx::query(
        "INSERT INTO scrape_pages(page_key,source,url,cursor) \
         SELECT incoming.page_key,incoming.source,incoming.url,incoming.cursor::jsonb \
         FROM unnest($1::text[],$2::text[],$3::text[],$4::text[]) WITH ORDINALITY \
           AS incoming(page_key,source,url,cursor,position) ORDER BY incoming.position \
         ON CONFLICT(source,page_key) DO UPDATE \
           SET status='pending',attempts=0,last_error=NULL,cursor=EXCLUDED.cursor WHERE FALSE",
    )
    .bind(keys)
    .bind(sources)
    .bind(urls)
    .bind(cursors)
    .execute(connection)
    .await?
    .rows_affected())
}
