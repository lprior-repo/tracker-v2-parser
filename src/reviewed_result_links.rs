mod manifest;
mod persist;

use anyhow::Result;
use sqlx::PgConnection;

use crate::durable::operations::{Artifact, Workspace};

pub use manifest::Prepared;
pub use persist::Report;

pub fn prepare(workspace: &Workspace, artifact: &Artifact) -> Result<Prepared> {
    manifest::prepare(workspace, artifact)
}

pub async fn apply(connection: &mut PgConnection, prepared: Prepared) -> Result<Report> {
    persist::apply(connection, prepared).await
}
