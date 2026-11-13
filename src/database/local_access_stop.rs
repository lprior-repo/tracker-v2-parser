use anyhow::{Result, ensure};
use sqlx::{Executor, PgPool, Postgres};

use crate::acquisition::local_stop::{LocalContactStop, public_origin};

pub async fn allows_public<'e, E>(executor: E, address: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let Some(origin) = public_origin(address) else {
        return Ok(false);
    };
    let rows = sqlx::query_as::<_, LocalContactStop>(include_str!("local_access_stop.sql"))
        .bind(&origin)
        .fetch_all(executor)
        .await?;
    ensure!(
        rows.len() <= 1000,
        "too many local contact stops; review required"
    );
    if rows.is_empty() || !rows.iter().all(|row| row.qualifies(&origin)) {
        return Ok(false);
    }
    Ok(true)
}

pub async fn permits_derived(pool: &PgPool, page: i64, address: &str) -> Result<bool> {
    let Some(origin) = public_origin(address) else {
        return Ok(false);
    };
    let detail = format!("source access stopped at {origin} (HTTP None); human review required");
    let local: bool = sqlx::query_scalar(include_str!("local_derived_stop.sql"))
        .bind(page)
        .bind(address)
        .bind(detail)
        .bind(origin)
        .fetch_one(pool)
        .await?;
    Ok(local && allows_public(pool, address).await?)
}

#[cfg(test)]
mod tests;
