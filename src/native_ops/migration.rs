mod archive;
mod backup;
mod bootstrap;
mod database;
mod exports;
mod files;
mod manifest;
mod paths;
mod references;
mod restore;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum MigrationCommand {
    Backup(BackupArgs),
    Restore(RestoreArgs),
}

#[derive(Debug, Args)]
pub struct BackupArgs {
    #[arg(long)]
    pub database: String,
    #[arg(long)]
    pub data_dir: PathBuf,
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long)]
    pub artifacts: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long)]
    pub export_file: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    #[arg(long)]
    pub archive: PathBuf,
    #[arg(long)]
    pub database: String,
    #[arg(long)]
    pub data_dir: PathBuf,
    #[arg(long)]
    pub bootstrap_macos: bool,
    #[arg(long)]
    pub create_with_admin_env: Option<String>,
}

#[tracing::instrument(skip_all)]
pub async fn run(command: MigrationCommand) -> Result<()> {
    match command {
        MigrationCommand::Backup(args) => backup::run(args).await,
        MigrationCommand::Restore(args) => restore::run(args).await,
    }
}
