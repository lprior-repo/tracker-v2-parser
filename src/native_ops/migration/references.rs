use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use futures::TryStreamExt;
use serde::Deserialize;
use sqlx::PgConnection;

use super::{
    files,
    manifest::{Fingerprint, MAX_FILES, digest},
};
use crate::durable::page::{ArtifactRef, CaptureRef};

#[tracing::instrument(skip_all)]
pub(super) async fn collect(connection: &mut PgConnection) -> Result<Vec<ArtifactRef>> {
    let rows = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT capture->'body' FROM public.durable_page_attempts WHERE capture IS NOT NULL \
         UNION ALL SELECT parsed->'body' FROM public.durable_page_attempts WHERE parsed IS NOT NULL \
         UNION ALL SELECT capture->'body' FROM public.durable_page_receipts \
         UNION ALL SELECT parsed->'body' FROM public.durable_page_receipts",
    )
    .fetch(connection)
    .map_err(anyhow::Error::from);
    let references = rows
        .try_fold(BTreeMap::new(), |mut references, value| async move {
            let reference: ArtifactRef =
                serde_json::from_value(value.context("durable artifact reference has no body")?)?;
            ensure!(
                digest(&reference.sha256) && reference.bytes <= 64 * 1024 * 1024,
                "invalid durable artifact reference"
            );
            ensure!(
                references.len() < MAX_FILES || references.contains_key(&reference.sha256),
                "too many referenced artifacts"
            );
            if let Some(old) = references.insert(reference.sha256, reference.bytes) {
                ensure!(
                    old == reference.bytes,
                    "conflicting durable artifact lengths"
                );
            }
            Ok::<_, anyhow::Error>(references)
        })
        .await?;
    Ok(references
        .into_iter()
        .map(|(sha256, bytes)| ArtifactRef { sha256, bytes })
        .collect())
}

pub(super) fn validate(root: &Path, required: &[ArtifactRef], files: &[Fingerprint]) -> Result<()> {
    ensure!(required.len() <= MAX_FILES, "too many required artifacts");
    let index = files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    required
        .iter()
        .try_for_each(|reference| validate_blob(reference, &index))?;
    files
        .iter()
        .filter(|file| file.path.starts_with("durable-artifacts/"))
        .try_for_each(|file| {
            let path = root.join(&file.path);
            match path.extension().and_then(|value| value.to_str()) {
                Some("capture") => {
                    let manifest: CaptureManifest = files::load(&path, 32_768)?;
                    ids(&path, &manifest.attempt_id, &manifest.operation_id)?;
                    validate_blob(&manifest.capture.body, &index)
                }
                Some("stop") => {
                    let manifest: StopManifest = files::load(&path, 8192)?;
                    ids(&path, &manifest.attempt_id, &manifest.operation_id)?;
                    let origin = url::Url::parse(&manifest.origin)?;
                    ensure!(
                        matches!(origin.scheme(), "http" | "https")
                            && origin.origin().ascii_serialization() == manifest.origin
                            && manifest.observed_at_ms >= 0
                            && manifest
                                .status
                                .is_none_or(|status| (100..=599).contains(&status)),
                        "invalid retained access stop metadata"
                    );
                    Ok(())
                }
                _ => Ok(()),
            }
        })
}

fn validate_blob(reference: &ArtifactRef, index: &BTreeMap<&str, &Fingerprint>) -> Result<()> {
    ensure!(
        digest(&reference.sha256) && reference.bytes <= 64 * 1024 * 1024,
        "invalid retained artifact reference"
    );
    let path = format!("durable-artifacts/{}.blob", reference.sha256);
    let file = index
        .get(path.as_str())
        .context("referenced durable artifact is missing from selected root")?;
    ensure!(
        file.sha256 == reference.sha256 && file.bytes == reference.bytes,
        "referenced durable artifact hash or length differs"
    );
    Ok(())
}

fn ids(path: &Path, attempt: &str, operation: &str) -> Result<()> {
    ensure!(
        digest(attempt)
            && digest(operation)
            && path.file_stem().and_then(|name| name.to_str()) == Some(attempt),
        "retained manifest identity differs from filename"
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureManifest {
    attempt_id: String,
    operation_id: String,
    capture: CaptureRef,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopManifest {
    attempt_id: String,
    operation_id: String,
    origin: String,
    status: Option<u16>,
    observed_at_ms: i64,
}
