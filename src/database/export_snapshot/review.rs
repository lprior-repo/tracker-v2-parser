use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};

use super::super::{Database, ExportSnapshot};
use crate::export::{ExportLayout, ExportOptions, ReviewRow, review};

impl ExportSnapshot<'_> {
    #[tracing::instrument(skip(self))]
    pub async fn review_records(
        &mut self,
        after_id: i64,
        max_rows: usize,
    ) -> Result<Vec<ReviewRow>> {
        ensure!(
            after_id >= 0 && (1..=1000).contains(&max_rows),
            "invalid bounded review cursor/batch"
        );
        ensure!(self.usable, super::ExportSnapshotInvalidated);
        self.usable = false;
        let rows: Vec<(Option<String>, i64)> = sqlx::query_as(include_str!("review.sql"))
            .bind(&self.states)
            .bind(&self.years)
            .bind(after_id)
            .bind(i64::try_from(max_rows)?)
            .fetch_all(&mut *self.transaction)
            .await
            .context("loading acquisition review snapshot")?;
        let rows = rows
            .into_iter()
            .map(|(row, bytes)| {
                let row = row.with_context(|| {
                    format!("review checkpoint row exceeds 64 KiB: {bytes} bytes")
                })?;
                serde_json::from_str::<ReviewRow>(&row)?.sanitize()
            })
            .collect::<Result<Vec<_>>>()?;
        self.usable = true;
        Ok(rows)
    }
}

pub(super) async fn collect(snapshot: &mut ExportSnapshot<'_>) -> Result<Vec<ReviewRow>> {
    stream::try_unfold((snapshot, 0_i64), |(snapshot, cursor)| async move {
        let rows = snapshot.review_records(cursor, 1000).await?;
        let Some(last) = rows.last() else {
            return Ok::<_, anyhow::Error>(None);
        };
        let cursor = last.checkpoint_id;
        Ok(Some((rows, (snapshot, cursor))))
    })
    .take(review::MAX_ROWS / 1000 + 1)
    .try_fold(Vec::new(), |mut all, rows| async move {
        all.extend(rows);
        review::validate(&all)?;
        Ok(all)
    })
    .await
}

impl Database {
    #[tracing::instrument(skip_all)]
    pub async fn write_export_with_review(
        &self,
        output: &Path,
        options: &ExportOptions,
        layout: ExportLayout,
        enabled: bool,
    ) -> Result<Vec<PathBuf>> {
        if !enabled {
            return self.write_export_with_layout(output, options, layout).await;
        }
        ensure!(
            layout == ExportLayout::SingleSheet,
            "Review requires single-sheet athlete layout"
        );
        let mut snapshot = self
            .begin_export_snapshot(&options.states, &options.graduation_years)
            .await?;
        let rows = collect(&mut snapshot).await?;
        super::writer::write_with_review(snapshot, output, options, layout, Some(rows)).await
    }
}
