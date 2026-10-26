use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions, sqlite::SqliteJournalMode};

#[tracing::instrument(skip(database_url))]
pub async fn check_database(database_url: &str) -> Result<String> {
    let options = SqliteConnectOptions::from_str(database_url)
        .context("parsing DATABASE_URL as a SQLite connection URL")?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .context("opening SQLite database; its parent directory must already exist")?;
    let result = check_connection(&mut connection).await;
    let close_result = connection.close().await.context("closing SQLite database");
    let version = result?;
    close_result?;
    Ok(version)
}

#[tracing::instrument(skip(connection))]
async fn check_connection(connection: &mut SqliteConnection) -> Result<String> {
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut *connection)
        .await
        .context("checking SQLite database integrity")?;
    if integrity != "ok" {
        bail!("SQLite integrity check failed: {integrity}");
    }
    sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(connection)
        .await
        .context("reading SQLite version")
}
