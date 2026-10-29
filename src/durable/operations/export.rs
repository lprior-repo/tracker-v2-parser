mod freeze;
mod publication;
mod render;
mod review;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{Artifact, OperationSpec, Workspace};
use crate::{config::Config, model::AthleteRecord};

pub use freeze::freeze_export;
pub use publication::publish_export;
pub use render::{prepare_export_chunk, render_export, verify_export};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenExport {
    pub manifest: Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenChunk {
    pub artifact: Artifact,
    pub athlete_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenPlan {
    pub binding: String,
    pub generation: u32,
    pub config: Config,
    pub chunks: Vec<FrozenChunk>,
    pub rows: usize,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_rows: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_bytes: Option<usize>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    review: Vec<crate::export::ReviewRow>,
    records: Vec<AthleteRecord>,
    issues: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedExport {
    pub binding: String,
    pub frozen_manifest: Artifact,
    pub workbook: Artifact,
    pub rows: usize,
    pub columns: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_rows: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedExport {
    pub binding: String,
    pub workbook: Artifact,
    pub manifest: Artifact,
    pub canonical_pointer: std::path::PathBuf,
    pub promoted: bool,
}

pub fn frozen_plan(workspace: &Workspace, frozen: &FrozenExport) -> Result<FrozenPlan> {
    let plan: FrozenPlan = serde_json::from_slice(&workspace.read(&frozen.manifest)?)?;
    ensure!(
        plan.binding == workspace.binding(),
        "frozen export operation binding mismatch"
    );
    workspace.validate_spec(&OperationSpec::Export {
        config: plan.config.clone(),
    })?;
    ensure!(
        plan.chunks.len() <= 10000 && plan.bytes <= crate::export::MAX_EXPORT_BYTES,
        "frozen export exceeds capacity"
    );
    let (bytes, rows) =
        plan.chunks
            .iter()
            .try_fold((0_u64, 0_usize), |(bytes, rows), chunk| -> Result<_> {
                ensure!(
                    chunk.artifact.bytes <= super::artifacts::MAX_ARTIFACT_BYTES,
                    "frozen chunk exceeds 64 MiB artifact capacity"
                );
                Ok((
                    bytes
                        .checked_add(chunk.artifact.bytes)
                        .ok_or_else(|| anyhow::anyhow!("frozen artifact bytes overflow"))?,
                    rows.checked_add(chunk.athlete_ids.len())
                        .ok_or_else(|| anyhow::anyhow!("frozen athlete count overflow"))?,
                ))
            })?;
    ensure!(
        bytes == plan.bytes && rows == plan.rows,
        "frozen snapshot totals mismatch"
    );
    plan.chunks
        .iter()
        .flat_map(|chunk| chunk.athlete_ids.iter().copied())
        .try_fold(0_i64, |previous, id| {
            ensure!(
                id > previous,
                "frozen snapshot athlete IDs must be globally increasing"
            );
            Ok::<_, anyhow::Error>(id)
        })?;
    Ok(plan)
}

fn options(config: &Config) -> crate::export::ExportOptions {
    crate::export::ExportOptions {
        states: config.states.clone(),
        graduation_years: config.graduation_years.clone(),
        rows_per_file: config.export.rows_per_file,
        per_athlete_sheets: false,
    }
}
