use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};

use super::super::{Database, ExportSnapshot};
use super::query::{self, Projection};
use crate::export::{ExportLayout, ExportOptions, ReviewRow, review};

#[derive(sqlx::FromRow)]
struct Candidate {
    checkpoint_id: i64,
    document: Option<String>,
    bytes: i64,
    athlete_id: Option<i64>,
}

impl Candidate {
    fn decode(self) -> Result<ReviewRow> {
        let document = self.document.with_context(|| {
            format!("review checkpoint row exceeds 64 KiB: {} bytes", self.bytes)
        })?;
        let row = serde_json::from_str::<ReviewRow>(&document)?;
        ensure!(
            row.checkpoint_id == self.checkpoint_id,
            "review checkpoint document must match its keyset identity"
        );
        row.sanitize()
    }
}

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
        let rows = self.read_review_records(after_id, max_rows).await?;
        self.usable = true;
        Ok(rows)
    }

    async fn read_review_records(
        &mut self,
        after_id: i64,
        max_rows: usize,
    ) -> Result<Vec<ReviewRow>> {
        stream::try_unfold(
            (
                self,
                after_id,
                max_rows,
                query::statement(Projection::Workbook),
            ),
            |(snapshot, cursor, remaining, statement)| async move {
                if remaining == 0 {
                    return Ok::<_, anyhow::Error>(None);
                }
                let candidates = snapshot.read_review_candidates(cursor, remaining).await?;
                let Some(last) = candidates.last() else {
                    return Ok(None);
                };
                let cursor = last.checkpoint_id;
                let exhausted = candidates.len() < remaining;
                let rows = snapshot
                    .filter_review_candidates(candidates, Arc::clone(&statement))
                    .await?;
                let remaining = if exhausted {
                    0
                } else {
                    remaining
                        .checked_sub(rows.len())
                        .context("review row budget underflow")?
                };
                Ok(Some((rows, (snapshot, cursor, remaining, statement))))
            },
        )
        .try_fold(Vec::new(), |mut all, rows| async move {
            all.extend(rows);
            Ok(all)
        })
        .await
    }

    async fn read_review_candidates(
        &mut self,
        after_id: i64,
        max_rows: usize,
    ) -> Result<Vec<Candidate>> {
        let rows = sqlx::query_as::<_, Candidate>(include_str!("review.sql"))
            .bind(&self.states)
            .bind(&self.years)
            .bind(after_id)
            .bind(i64::try_from(max_rows)?)
            .fetch_all(&mut *self.transaction)
            .await
            .context("loading acquisition review snapshot")?;
        ensure!(
            rows.len() <= max_rows
                && rows.iter().all(|row| row.checkpoint_id > after_id)
                && rows.windows(2).all(|pair| {
                    matches!(pair, [left, right] if left.checkpoint_id < right.checkpoint_id)
                }),
            "review keyset must be bounded and strictly ascending"
        );
        Ok(rows)
    }

    async fn filter_review_candidates(
        &mut self,
        candidates: Vec<Candidate>,
        statement: Arc<str>,
    ) -> Result<Vec<ReviewRow>> {
        let (_, rows, _) = stream::iter(candidates)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(
                (self, Vec::new(), statement),
                |(snapshot, mut rows, statement), candidate| async move {
                    let public_marks = if let Some(id) = candidate.athlete_id {
                        let (record, _) = snapshot
                            .read_bounded_record(id, Arc::clone(&statement))
                            .await?;
                        tokio::task::spawn_blocking(move || {
                            crate::export::has_public_marks(&record)
                        })
                        .await
                        .context("joining bounded public mark projection")??
                    } else {
                        false
                    };
                    if !public_marks {
                        rows.push(candidate.decode()?);
                    }
                    Ok((snapshot, rows, statement))
                },
            )
            .await?;
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
