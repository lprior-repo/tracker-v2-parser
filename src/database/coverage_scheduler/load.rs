use std::{collections::HashSet, sync::Arc};

use anyhow::{Result, ensure};
use futures::TryStreamExt;
use sqlx::PgConnection;

use crate::scope::LOWER_48;

use super::index::Index;
use super::model::{Auxiliary, Fact, Key, Operation, Owned};

pub(super) async fn facts(
    connection: &mut PgConnection,
    key: &Key,
    ids: Option<&[i64]>,
) -> Result<Vec<Fact>> {
    if ids.is_some_and(<[i64]>::is_empty) {
        return Ok(Vec::new());
    }
    let statement = if ids.is_some() {
        concat!(
            include_str!("facts.sql"),
            " AND p.id=ANY($4::bigint[]) ORDER BY p.id LIMIT 250001"
        )
    } else {
        concat!(include_str!("facts.sql"), " ORDER BY p.id LIMIT 250001")
    };
    let query = sqlx::query_as::<_, Fact>(statement)
        .bind(&key.states)
        .bind(&key.years)
        .bind(LOWER_48.as_slice());
    let query = if let Some(ids) = ids {
        query.bind(ids)
    } else {
        query
    };
    let (rows, _): (Vec<Fact>, usize) = query
        .fetch(connection)
        .map_err(anyhow::Error::from)
        .try_fold((Vec::new(), 0_usize), |(mut rows, total), row| async move {
            let total = total
                .checked_add(row.retained_bytes()?)
                .ok_or_else(|| anyhow::anyhow!("compiled coverage byte budget overflow"))?;
            ensure!(
                rows.len() < 250_000 && total <= 128 * 1024 * 1024,
                "compiled coverage exceeds 250000-page or128MiB input bound"
            );
            rows.try_reserve(1)?;
            rows.push(row);
            Ok((rows, total))
        })
        .await?;
    Ok(rows)
}

pub(super) async fn operations(
    connection: &mut PgConnection,
    key: &Key,
    ids: Option<&[i64]>,
) -> Result<Vec<Operation>> {
    if ids.is_some_and(<[i64]>::is_empty) {
        return Ok(Vec::new());
    }
    let Some(generation) = key.generation.as_deref() else {
        return Ok(Vec::new());
    };
    let statement = if ids.is_some() {
        "SELECT operation_id AS identity,page_id,state,cycle,cycle_attempts,retry_at_ms FROM durable_page_operations WHERE run_generation=$1 AND page_id=ANY($2::bigint[]) LIMIT 250001"
    } else {
        "SELECT operation_id AS identity,page_id,state,cycle,cycle_attempts,retry_at_ms FROM durable_page_operations WHERE run_generation=$1 LIMIT 250001"
    };
    let query = sqlx::query_as::<_, Operation>(statement).bind(generation);
    let query = if let Some(ids) = ids {
        query.bind(ids)
    } else {
        query
    };
    let rows = query.fetch_all(connection).await?;
    ensure!(
        rows.len() <= 250_000,
        "compiled retry state exceeds 250000-page memory bound"
    );
    Ok(rows)
}

pub(super) async fn auxiliary(
    connection: &mut PgConnection,
    official_urls: &[String],
    authorities: &HashSet<String>,
) -> Result<Auxiliary> {
    let origins: Vec<String> = sqlx::query_scalar("SELECT DISTINCT regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')),CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') FROM scrape_pages blocked CROSS JOIN LATERAL unnest(ARRAY[blocked.url,blocked.cursor->>'access_stop_url']) stop(url) WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts' AND substring(stop.url from '(?i)^https?://([^/?#]+)') IS NOT NULL LIMIT 250001")
        .fetch_all(&mut *connection).await?;
    let official: Vec<(String, String)> = sqlx::query_as(include_str!("official.sql"))
        .bind(official_urls)
        .fetch_all(connection)
        .await?;
    ensure!(
        origins.len() <= 250_000 && official.len() <= 250_000,
        "compiled contact coverage exceeds memory bound"
    );
    Ok(Auxiliary {
        stopped: origins
            .into_iter()
            .filter(|origin| authorities.contains(origin))
            .collect::<HashSet<_>>(),
        official: official.into_iter().fold(
            std::collections::HashMap::new(),
            |mut claims, (school, url)| {
                claims
                    .entry(school)
                    .or_insert_with(HashSet::new)
                    .insert(url);
                claims
            },
        ),
    })
}

pub(super) async fn owned(connection: &mut PgConnection, ids: &[i64]) -> Result<Vec<Owned>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as("SELECT DISTINCT p.id,p.source,p.cursor->>'kind' AS kind,p.cursor->'context'->>'school_id' AS school,p.cursor->'context'->>'athlete_id' AS athlete FROM scrape_pages p WHERE p.id=ANY($1::bigint[]) AND EXISTS(SELECT 1 FROM durable_page_operations d WHERE d.page_id=p.id AND d.state='active')")
        .bind(ids).fetch_all(connection).await?)
}

pub(super) fn merge_facts(
    previous: Option<&Arc<Index<Arc<Fact>>>>,
    ids: Option<&[i64]>,
    rows: Vec<Fact>,
) -> Result<Arc<Index<Arc<Fact>>>> {
    if rows.is_empty()
        && ids.is_some_and(|ids| {
            ids.iter()
                .all(|id| previous.is_none_or(|rows| !rows.contains_key(*id)))
        })
    {
        return Ok(previous.map_or_else(|| Arc::new(Index::default()), Arc::clone));
    }
    let mut next = previous.map_or_else(Index::default, |rows| Index::clone(rows));
    ids.into_iter()
        .flatten()
        .try_for_each(|id| next.remove(*id))?;
    rows.into_iter().try_for_each(|row| {
        let bytes = row.retained_bytes()?;
        next.insert(row.id, Arc::new(row), bytes)
    })?;
    Ok(Arc::new(next))
}

pub(super) fn merge_operations(
    previous: Option<&Arc<Index<Arc<Operation>>>>,
    ids: Option<&[i64]>,
    rows: Vec<Operation>,
) -> Result<Arc<Index<Arc<Operation>>>> {
    if rows.is_empty()
        && ids.is_some_and(|ids| {
            ids.iter()
                .all(|id| previous.is_none_or(|rows| !rows.contains_key(*id)))
        })
    {
        return Ok(previous.map_or_else(|| Arc::new(Index::default()), Arc::clone));
    }
    let mut next = previous.map_or_else(Index::default, |rows| Index::clone(rows));
    ids.into_iter()
        .flatten()
        .try_for_each(|id| next.remove(*id))?;
    rows.into_iter().try_for_each(|row| {
        let bytes = std::mem::size_of::<Operation>()
            .checked_add(row.identity.len())
            .and_then(|bytes| bytes.checked_add(row.state.len()))
            .ok_or_else(|| anyhow::anyhow!("operation byte budget overflow"))?;
        next.insert(row.page_id, Arc::new(row), bytes)
    })?;
    Ok(Arc::new(next))
}
