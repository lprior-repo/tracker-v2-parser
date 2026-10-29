mod link;
mod manifest;

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Report {
    pub canonical_athlete_id: i64,
    pub profile_count: usize,
    pub result_count: i64,
    pub already_applied: bool,
    pub manifest_sha256: String,
}

pub async fn import_transaction(
    connection: &mut sqlx::PgConnection,
    path: &Path,
    expected: &str,
) -> Result<Report> {
    let path = path.to_path_buf();
    let expected = expected.to_owned();
    let reviewed = tokio::task::spawn_blocking(move || manifest::load(&path, &expected)).await??;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtext(current_schema()),194832741)",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !acquired {
        return Err(crate::durable::AcquisitionBusy.into());
    }
    link::apply(connection, &reviewed).await
}
