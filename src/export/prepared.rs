use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{ExportLayout, ExportOptions, ReviewRow, StagedExport, selection, single_sheet};
use crate::model::AthleteRecord;

pub fn verify_chunks(path: &std::path::Path, chunks: &[PreparedChunk]) -> Result<(usize, usize)> {
    verify_chunks_with_review(path, chunks, None)
}

pub fn verify_chunks_with_review(
    path: &std::path::Path,
    chunks: &[PreparedChunk],
    review_rows: Option<&[ReviewRow]>,
) -> Result<(usize, usize)> {
    super::prepared_verify::verify_source(
        path,
        chunks.len(),
        |index| {
            chunks
                .get(index)
                .cloned()
                .context("prepared chunk index absent")
        },
        review_rows,
    )
}

pub fn verify_chunk_source(
    path: &std::path::Path,
    chunk_count: usize,
    load: impl Fn(usize) -> Result<PreparedChunk>,
    review_rows: Option<&[ReviewRow]>,
) -> Result<(usize, usize)> {
    super::prepared_verify::verify_source(path, chunk_count, load, review_rows)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChunk {
    pub athlete_ids: Vec<i64>,
    pub(super) rows: Vec<single_sheet::Prepared>,
}

pub fn prepare_chunk(records: &[AthleteRecord], options: &ExportOptions) -> Result<PreparedChunk> {
    ensure!(
        records.len() <= 1000,
        "prepared chunk exceeds 1000 athletes"
    );
    let records = records
        .iter()
        .filter(|record| {
            selection::confirmed_state(record, options)
                && record
                    .graduation_year
                    .is_some_and(|year| options.graduation_years.contains(&year))
        })
        .collect::<Vec<_>>();
    let rows = records
        .iter()
        .map(|record| single_sheet::prepare(record))
        .collect::<Result<Vec<_>>>()?;
    Ok(PreparedChunk {
        athlete_ids: records.iter().map(|record| record.id).collect(),
        rows,
    })
}

impl StagedExport {
    pub fn stage_prepared(&mut self, chunk: PreparedChunk) -> Result<()> {
        ensure!(
            self.layout == ExportLayout::SingleSheet,
            "prepared chunks require SingleSheet layout"
        );
        ensure!(
            self.started_layout
                .is_none_or(|layout| layout == self.layout),
            "cannot change export layout after staging"
        );
        ensure!(
            chunk.athlete_ids.len() == chunk.rows.len() && chunk.rows.len() <= 1000,
            "prepared chunk identity/row count mismatch"
        );
        ensure!(
            chunk.athlete_ids.iter().all(|id| *id > 0),
            "prepared athlete IDs must be positive"
        );
        self.started_layout = Some(self.layout);
        self.single_sheet
            .append_prepared(chunk.rows, self.options.rows_per_file)
    }
}
