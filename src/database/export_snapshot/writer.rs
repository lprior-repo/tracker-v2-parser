use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures::TryStreamExt;

use super::{EXPORT_BATCH_ROWS, ExportSnapshot, parts};
use crate::export::{ExportOptions, StagedExport};

pub async fn write(
    snapshot: ExportSnapshot<'_>,
    output: &Path,
    options: &ExportOptions,
    layout: crate::export::ExportLayout,
) -> Result<Vec<PathBuf>> {
    write_with_review(snapshot, output, options, layout, None).await
}

pub(super) async fn write_with_review(
    snapshot: ExportSnapshot<'_>,
    output: &Path,
    options: &ExportOptions,
    layout: crate::export::ExportLayout,
    review: Option<Vec<crate::export::ReviewRow>>,
) -> Result<Vec<PathBuf>> {
    let staged = StagedExport::new(output, options)?.with_layout(layout);
    let staged = if let Some(rows) = review {
        staged.with_review(rows)?
    } else {
        staged
    };
    let rows = options.rows_per_file.min(EXPORT_BATCH_ROWS);
    let staged = parts(snapshot, rows)
        .try_fold(staged, |mut staged, (records, issues)| async move {
            tokio::task::spawn_blocking(move || {
                staged.stage_part(&records, &issues)?;
                Ok::<_, anyhow::Error>(staged)
            })
            .await
            .context("joining bounded workbook staging")?
        })
        .await?;
    tokio::task::spawn_blocking(move || staged.finish())
        .await
        .context("joining export publication")?
}
