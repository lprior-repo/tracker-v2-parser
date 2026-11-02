use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use sqlx::PgConnection;

use super::{Database, Job};
use crate::durable::blocking::BlockingStages;

mod auxiliary;
mod compile;
mod delta;
mod index;
mod load;
mod membership;
mod model;
#[cfg(test)]
mod order_tests;
mod refresh;
#[cfg(test)]
mod tests;
mod update;

use model::{Key, Snapshot};
use refresh::run as refresh;

#[derive(Default)]
pub(super) struct Cache {
    snapshot: Mutex<Option<Arc<Snapshot>>>,
    compiler: BlockingStages,
}

impl fmt::Debug for Cache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CoverageCache")
            .finish_non_exhaustive()
    }
}

impl Cache {
    fn current(&self, key: &Key) -> Result<Option<Arc<Snapshot>>> {
        Ok(self
            .snapshot
            .lock()
            .map_err(|_| anyhow!("coverage cache is poisoned"))?
            .as_ref()
            .filter(|snapshot| snapshot.key == *key)
            .map(Arc::clone))
    }

    fn publish(&self, snapshot: Arc<Snapshot>) -> Result<()> {
        *self
            .snapshot
            .lock()
            .map_err(|_| anyhow!("coverage cache is poisoned"))? = Some(snapshot);
        Ok(())
    }

    pub async fn drain(&self) -> Result<()> {
        self.compiler.drain().await
    }
}

pub(super) struct Request<'a> {
    pub states: &'a [String],
    pub years: &'a [i32],
    pub generation: Option<&'a str>,
    pub max_attempts: i32,
    pub contacts: bool,
    pub now_ms: i64,
    pub owned: &'a [i64],
    pub allow_deferred: bool,
    pub exhausted: bool,
}

impl Request<'_> {
    fn key(&self) -> Key {
        Key {
            states: self.states.to_vec(),
            years: self.years.to_vec(),
            generation: self.generation.map(str::to_owned),
            max_attempts: self.max_attempts,
            contacts: self.contacts,
        }
    }
}

#[tracing::instrument(skip_all)]
async fn read_snapshot(
    connection: &mut PgConnection,
    database: &Database,
    key: Key,
    previous: Option<&Arc<Snapshot>>,
) -> Result<Arc<Snapshot>> {
    sqlx::raw_sql(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL work_mem='16MB'",
    )
    .execute(&mut *connection)
    .await?;
    let visibility: String = sqlx::query_scalar("SELECT pg_current_snapshot()::text")
        .fetch_one(&mut *connection)
        .await?;
    let prepared = refresh(connection, &database.coverage, &key, previous).await?;
    Ok(Arc::new(Snapshot {
        key,
        visibility,
        prepared,
    }))
}

#[tracing::instrument(skip_all)]
pub(super) async fn prepare(database: &Database, request: Request<'_>) -> Result<()> {
    let started = Instant::now();
    let key = request.key();
    let previous = database.coverage.current(&key)?;
    let mut transaction = database.pool.begin().await?;
    let result = read_snapshot(&mut transaction, database, key, previous.as_ref()).await;
    let snapshot = super::finish(transaction, result).await?;
    let pages = snapshot.prepared.facts.len();
    database.coverage.publish(snapshot)?;
    tracing::info!(
        elapsed_us = started.elapsed().as_micros(),
        pages,
        cold = previous.is_none(),
        "coverage preparation complete"
    );
    Ok(())
}

#[tracing::instrument(skip_all)]
pub(super) async fn select(database: &Database, request: Request<'_>) -> Result<Option<Job>> {
    let started = Instant::now();
    let key = request.key();
    let previous = database.coverage.current(&key)?;
    let mut transaction = database.pool.begin().await?;
    let result = async {
        let snapshot = read_snapshot(&mut transaction, database, key, previous.as_ref()).await?;
        let owned = load::owned(
            &mut transaction,
            if request.generation.is_some() {
                request.owned
            } else {
                &[]
            },
        )
        .await?;
        let selected = snapshot.selected(
            request.now_ms,
            request.allow_deferred,
            &owned,
            request.exhausted,
        );
        let target = if let Some(id) = selected {
            let cursor: serde_json::Value =
                sqlx::query_scalar("SELECT cursor FROM scrape_pages WHERE id=$1")
                    .bind(id)
                    .fetch_one(&mut *transaction)
                    .await?;
            Some(Job {
                id,
                target: serde_json::from_value(cursor)
                    .context("decoding compiled coverage target")?,
            })
        } else {
            None
        };
        Ok((snapshot, target))
    }
    .await;
    let (snapshot, target) = super::finish(transaction, result).await?;
    let candidates = snapshot.prepared.candidates.len();
    database.coverage.publish(snapshot)?;
    tracing::debug!(
        elapsed_us = started.elapsed().as_micros(),
        candidates,
        cold = previous.is_none(),
        "compiled coverage selection"
    );
    Ok(target)
}
