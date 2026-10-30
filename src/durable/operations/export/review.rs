use anyhow::{Context, Result, ensure};

use super::{Batch, FrozenPlan, Workspace};
use crate::export::{ReviewRow, review};

pub(super) fn rows(workspace: &Workspace, plan: &FrozenPlan) -> Result<Option<Vec<ReviewRow>>> {
    if !plan.config.export.review_sheet {
        ensure!(
            plan.review_rows.is_none() && plan.review_bytes.is_none(),
            "disabled Review has frozen review count"
        );
        return Ok(None);
    }
    ensure!(
        plan.review_bytes
            .is_some_and(|bytes| bytes <= review::MAX_BYTES),
        "frozen review byte bound missing/exceeded"
    );
    let expected = plan
        .review_rows
        .context("enabled Review is missing frozen row count")?;
    ensure!(
        expected <= review::MAX_ROWS,
        "frozen review exceeds 100000 rows"
    );
    let rows = plan
        .chunks
        .iter()
        .try_fold(Vec::new(), |mut rows, chunk| -> Result<_> {
            let batch: Batch = serde_json::from_slice(&workspace.read(&chunk.artifact)?)?;
            rows.extend(batch.review);
            review::validate(&rows)?;
            Ok(rows)
        })?;
    let bytes = rows.iter().try_fold(0_usize, |bytes, row| -> Result<_> {
        bytes
            .checked_add(serde_json::to_vec(row)?.len())
            .context("frozen review byte count overflow")
    })?;
    ensure!(
        Some(bytes) == plan.review_bytes,
        "frozen review byte count differs from manifest"
    );
    ensure!(
        rows.len() == expected,
        "frozen review row count differs from manifest"
    );
    Ok(Some(rows))
}
