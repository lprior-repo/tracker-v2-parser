use std::env;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "track-scrapper-v2", version)]
pub struct Cli {
    #[arg(long, global = true)]
    database_url: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Run the Restate-managed pipeline")]
    Durable(crate::durable::cli::DurableArgs),
}

impl Cli {
    async fn run(self) -> Result<()> {
        let Command::Durable(args) = self.command;
        args.run(resolve_database_url(self.database_url)?).await
    }
}

fn resolve_database_url(argument: Option<String>) -> Result<String> {
    match argument {
        Some(url) if !url.is_empty() => Ok(url),
        Some(_) => Err(anyhow!("database URL is empty")),
        None => match env::var("DATABASE_URL") {
            Ok(url) if !url.is_empty() => Ok(url),
            Ok(_) => Err(anyhow!("DATABASE_URL is set but empty")),
            Err(env::VarError::NotPresent) => {
                Err(anyhow!("DATABASE_URL environment variable is not set"))
            }
            Err(env::VarError::NotUnicode(_)) => {
                Err(anyhow!("DATABASE_URL contains non-UTF-8 bytes"))
            }
        },
    }
}

pub async fn run() -> Result<()> {
    Cli::parse().run().await
}
