mod client;
mod commands;
#[cfg(test)]
mod tests;

use super::{
    model::CrawlRequest,
    operations::{OperationRequest, OperationSpec},
};
use anyhow::Result;
use clap::{Args, Subcommand};
pub use client::{read_request, send};
use commands::{start_crawl, submit_operation};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Args)]
pub struct DurableArgs {
    #[command(subcommand)]
    command: DurableCommand,
}

#[derive(Args, Debug, Clone, Copy, Default)]
pub(super) struct RunOverrides {
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=24))]
    hours: Option<u32>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    max_batches: Option<u32>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(15..=3_600))]
    cooldown_seconds: Option<u32>,
}

#[derive(Subcommand)]
enum DurableCommand {
    Crawl {
        #[arg(long, default_value = "config.toml")]
        config: PathBuf,
        #[arg(long)]
        generation: String,
        #[command(flatten)]
        run: RunOverrides,
        #[arg(long, default_value = "http://127.0.0.1:18095")]
        ingress: String,
    },
    Export {
        #[arg(long, default_value = "config.toml")]
        config: PathBuf,
        #[arg(long)]
        operation_id: String,
        #[arg(long, default_value = "http://127.0.0.1:18095")]
        ingress: String,
    },
    Contacts {
        #[arg(long, default_value = "config.toml")]
        config: PathBuf,
        #[arg(long)]
        operation_id: String,
        #[arg(long, default_value = "http://127.0.0.1:18095")]
        ingress: String,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:9085")]
        bind: SocketAddr,
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        published_workbook: Option<PathBuf>,
    },
    Submit {
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:18095")]
        ingress: String,
    },
    Operation {
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:18095")]
        ingress: String,
    },
    Status {
        #[arg(long)]
        key: String,
        #[arg(long, default_value = "http://127.0.0.1:18095")]
        ingress: String,
    },
}

impl DurableArgs {
    pub async fn run(self, database_url: String) -> Result<()> {
        match self.command {
            DurableCommand::Crawl {
                config,
                generation,
                run,
                ingress,
            } => start_crawl(&config, generation, run, &ingress).await,
            DurableCommand::Export {
                config,
                operation_id,
                ingress,
            } => {
                submit_operation(
                    &ingress,
                    OperationRequest {
                        operation_id,
                        spec: OperationSpec::Export {
                            config: crate::config::Config::load(&config)?,
                        },
                    },
                )
                .await
            }
            DurableCommand::Contacts {
                config,
                operation_id,
                ingress,
            } => {
                submit_operation(
                    &ingress,
                    OperationRequest {
                        operation_id,
                        spec: OperationSpec::WiaaDirectory {
                            config: crate::config::Config::load(&config)?,
                        },
                    },
                )
                .await
            }
            DurableCommand::Serve {
                bind,
                artifacts,
                published_workbook,
            } => super::serve(database_url, artifacts, bind, published_workbook).await,
            DurableCommand::Submit { request, ingress } => {
                let value: CrawlRequest = read_request(&request)?;
                value.validate(chrono::Utc::now().timestamp_millis())?;
                let key = value.workflow_key();
                let response = send(
                    &ingress,
                    &format!("restate/send/AthleticCrawl/{key}/run"),
                    None,
                    Some(&value),
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                Ok(())
            }
            DurableCommand::Operation { request, ingress } => {
                let value: OperationRequest = read_request(&request)?;
                submit_operation(&ingress, value).await
            }
            DurableCommand::Status { key, ingress } => {
                super::model::identifier(&key)?;
                let response = send(
                    &ingress,
                    &format!("restate/call/AthleticCrawl/{key}/status"),
                    None,
                    None::<&()>,
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                Ok(())
            }
        }
    }
}
