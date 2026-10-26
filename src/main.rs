#![recursion_limit = "256"]

use std::io::ErrorKind;

use anyhow::{Context, Result, anyhow};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    load_environment()?;
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow!("initializing logging: {error}"))?;
    track_scrapper_v2::cli::run().await
}

fn load_environment() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(dotenvy::Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("loading local .env configuration"),
    }
}
