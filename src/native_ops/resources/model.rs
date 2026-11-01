use std::path::PathBuf;

use anyhow::{Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Debug, Subcommand)]
pub enum ResourceCommand {
    Snapshot(SampleArgs),
    Plan(PlanArgs),
    PrepareConfig(PrepareArgs),
}

#[derive(Debug, Args)]
pub struct SampleArgs {
    #[arg(
        long,
        help = "PostgreSQL database name; connection credentials remain in private libpq environment"
    )]
    pub database: String,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long)]
    pub capacity_path: PathBuf,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=60))]
    pub samples: u8,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=60))]
    pub interval_seconds: u8,
}

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long, value_enum, default_value_t = ConfigurationProfile::Observability)]
    pub profile: ConfigurationProfile,
    #[arg(long, default_value_t = 16)]
    pub host_memory_gib: u16,
    #[arg(long, default_value_t = 8)]
    pub stack_memory_gib: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationProfile {
    Observability,
    Maintenance,
    CacheTrial,
}

impl SampleArgs {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=60).contains(&self.samples),
            "sample count must be 1..60"
        );
        ensure!(
            (1..=60).contains(&self.interval_seconds),
            "sample interval must be 1..60seconds"
        );
        ensure!(
            u16::from(self.samples).saturating_mul(u16::from(self.interval_seconds)) <= 300,
            "sample window exceeds 300 seconds"
        );
        ensure!(
            self.output.is_absolute() && self.capacity_path.is_absolute(),
            "resource paths must be absolute"
        );
        Ok(())
    }
}

#[derive(Debug, Args)]
pub struct PrepareArgs {
    #[arg(long)]
    pub prior_config: PathBuf,
    #[arg(long)]
    pub expected_prior_sha256: String,
    #[arg(long)]
    pub candidate_config: PathBuf,
    #[arg(
        long,
        help = "PostgreSQL database name; credentials remain in private libpq environment"
    )]
    pub database: String,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long, value_enum)]
    pub profile: ConfigurationProfile,
    #[arg(long, default_value_t = 16)]
    pub host_memory_gib: u16,
    #[arg(long, default_value_t = 8)]
    pub stack_memory_gib: u16,
}
