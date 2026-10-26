mod database;

use std::env::{self, VarError};

use anyhow::{Context, Result, anyhow};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .try_init()
        .map_err(|error| anyhow!("initializing logging: {error}"))?;
    run().await
}

#[tracing::instrument]
async fn run() -> Result<()> {
    let database_url = match env::var("DATABASE_URL") {
        Ok(value) => value,
        Err(VarError::NotPresent) => String::from("sqlite://track-scrapper.sqlite3"),
        Err(error) => return Err(error).context("reading DATABASE_URL"),
    };
    let version = database::check_database(&database_url).await?;
    tracing::info!(sqlite_version = %version, "SQLite database ready");
    Ok(())
}
