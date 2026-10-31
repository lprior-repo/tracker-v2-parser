mod manifest;
mod persist;

use anyhow::Result;

use super::{Artifact, Workspace};

pub(super) use manifest::Reviewed;
pub(super) use persist::Report;

pub(super) fn prepare(workspace: &Workspace, artifact: &Artifact) -> Result<Reviewed> {
    let bytes = workspace.read(artifact)?;
    manifest::parse(workspace, &bytes, artifact.sha256.clone())
}

pub(super) async fn apply(
    connection: &mut sqlx::PgConnection,
    reviewed: Reviewed,
) -> Result<Report> {
    persist::apply(connection, reviewed).await
}
