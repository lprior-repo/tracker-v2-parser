#![forbid(unsafe_code)]

use anyhow::Result;
use clap::{Parser, Subcommand};
use track_scrapper_v2::native_ops::{migration, recovery, resources, worker};

#[derive(Debug, Parser)]
#[command(
    name = "native-ops",
    about = "Native durable pipeline installation and recovery"
)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    WorkerInstall(worker::WorkerInstallArgs),
    WorkerLaunch(worker::WorkerLaunchArgs),
    Migration {
        #[command(subcommand)]
        command: migration::MigrationCommand,
    },
    Recovery(recovery::RecoveryArgs),
    Resources {
        #[command(subcommand)]
        command: resources::ResourceCommand,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    match Arguments::parse().command {
        Command::WorkerInstall(args) => worker::run_install(args).await,
        Command::WorkerLaunch(args) => worker::run_launch(args).await,
        Command::Migration { command } => migration::run(command).await,
        Command::Recovery(args) => recovery::run(args).await,
        Command::Resources { command } => resources::run(command).await,
    }
}
