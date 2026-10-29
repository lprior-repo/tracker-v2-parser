use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{Artifact, Batch, FrozenExport, VerifiedExport, Workspace, frozen_plan, options};
use crate::export::{ExportLayout, StagedExport, prepare_chunk, verify_chunk_source};

#[derive(Serialize, Deserialize)]
struct ChunkReceipt {
    frozen_sha256: String,
    source: Artifact,
    prepared: Artifact,
}

#[derive(Serialize, Deserialize)]
struct RenderReceipt {
    frozen_sha256: String,
    workbook: Artifact,
}

fn receipt_name(index: usize) -> String {
    format!("prepared-chunk-{index}.json")
}

pub fn prepare_export_chunk(
    workspace: &Workspace,
    frozen: &FrozenExport,
    index: usize,
) -> Result<Artifact> {
    let plan = frozen_plan(workspace, frozen)?;
    let source = plan
        .chunks
        .get(index)
        .context("frozen export chunk index out of bounds")?;
    workspace.read(&source.artifact)?;
    if let Some(receipt) = workspace.load::<ChunkReceipt>(&receipt_name(index))? {
        ensure!(
            receipt.frozen_sha256 == frozen.manifest.sha256 && receipt.source == source.artifact,
            "prepared chunk binding mismatch"
        );
        workspace.read(&receipt.prepared)?;
        return Ok(receipt.prepared);
    }
    let batch: Batch = serde_json::from_slice(&workspace.read(&source.artifact)?)?;
    ensure!(
        batch
            .records
            .iter()
            .map(|record| record.id)
            .eq(source.athlete_ids.iter().copied()),
        "frozen batch athlete identity mismatch"
    );
    let chunk = prepare_chunk(&batch.records, &options(&plan.config))?;
    let prepared = workspace.record(&format!("prepared-rows-{index}.json"), &chunk)?;
    workspace.record(
        &receipt_name(index),
        &ChunkReceipt {
            frozen_sha256: frozen.manifest.sha256.clone(),
            source: source.artifact.clone(),
            prepared: prepared.clone(),
        },
    )?;
    Ok(prepared)
}

pub fn render_export(workspace: &Workspace, frozen: &FrozenExport) -> Result<Artifact> {
    let plan = frozen_plan(workspace, frozen)?;
    if let Some(receipt) = workspace.load::<RenderReceipt>("render-receipt.json")? {
        ensure!(
            receipt.frozen_sha256 == frozen.manifest.sha256,
            "render receipt snapshot mismatch"
        );
        workspace.read(&receipt.workbook)?;
        return Ok(receipt.workbook);
    }
    let generation = (1..=100_u32)
        .find(|number| {
            workspace
                .path(&format!("assembly-{number}.json"))
                .is_ok_and(|path| !path.exists())
        })
        .context("export exceeds 100 assembly recoveries")?;
    workspace.record(
        &format!("assembly-{generation}.json"),
        &serde_json::json!({"snapshot":frozen.manifest.sha256}),
    )?;
    let name = format!("assembly-{generation}.xlsx");
    let review_rows = super::review::rows(workspace, &plan)?;
    let mut export = StagedExport::new(&workspace.path(&name)?, &options(&plan.config))?
        .with_layout(ExportLayout::SingleSheet);
    if let Some(rows) = review_rows {
        export = export.with_review(rows)?;
    }
    plan.chunks
        .iter()
        .enumerate()
        .try_for_each(|(index, _)| -> Result<()> {
            let artifact = prepare_export_chunk(workspace, frozen, index)?;
            let chunk = serde_json::from_slice(&workspace.read(&artifact)?)?;
            export.stage_prepared(chunk)
        })?;
    export.finish()?;
    let workbook = workspace.inspect(&name)?;
    workspace.record(
        "render-receipt.json",
        &RenderReceipt {
            frozen_sha256: frozen.manifest.sha256.clone(),
            workbook: workbook.clone(),
        },
    )?;
    Ok(workbook)
}

pub fn verify_export(
    workspace: &Workspace,
    frozen: &FrozenExport,
    workbook: &Artifact,
) -> Result<VerifiedExport> {
    workspace.read(workbook)?;
    if let Some(verified) = workspace.load::<VerifiedExport>("verification-receipt.json")? {
        ensure!(
            verified.binding == workspace.binding()
                && verified.frozen_manifest == frozen.manifest
                && verified.workbook == *workbook,
            "verification receipt binding mismatch"
        );
        return Ok(verified);
    }
    let plan = frozen_plan(workspace, frozen)?;
    let review_rows = super::review::rows(workspace, &plan)?;
    let (rows, columns) = verify_chunk_source(
        &workbook.path,
        plan.chunks.len(),
        |index| {
            let artifact = prepare_export_chunk(workspace, frozen, index)?;
            Ok(serde_json::from_slice(&workspace.read(&artifact)?)?)
        },
        review_rows.as_deref(),
    )?;
    let verified = VerifiedExport {
        binding: workspace.binding().into(),
        frozen_manifest: frozen.manifest.clone(),
        workbook: workbook.clone(),
        rows,
        columns,
        review_rows: plan.review_rows,
    };
    workspace.record("verification-receipt.json", &verified)?;
    Ok(verified)
}
