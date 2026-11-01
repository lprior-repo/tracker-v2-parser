mod capacity;
mod delta;
mod host;
mod interval;
mod model;
mod plan;
mod prepare;
mod staging;
mod telemetry;

pub use capacity::{
    BackupCapacityReceipt, CapacityBudget, CapacityObservation, backup_preflight, observe_capacity,
};
pub use delta::counter_delta;
pub use model::{ConfigurationProfile, ResourceCommand, SampleArgs};
pub use plan::configuration_plan;
pub use prepare::{prepare_configuration, validate_observation_age};
pub use telemetry::{snapshot, snapshot_connection};

use anyhow::Result;

#[tracing::instrument(skip_all)]
pub async fn run(command: ResourceCommand) -> Result<()> {
    match command {
        ResourceCommand::Snapshot(args) => telemetry::run(args).await,
        ResourceCommand::Plan(args) => plan::run(&args),
        ResourceCommand::PrepareConfig(args) => prepare::run(args).await,
    }
}
