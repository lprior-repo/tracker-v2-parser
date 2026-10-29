use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{ExportLayout, ExportOptions, StagedExport, selection, single_sheet};
use crate::model::AthleteRecord;

pub fn verify_chunks(path: &std::path::Path, chunks: &[PreparedChunk]) -> Result<(usize, usize)> {
    use calamine::{Reader, open_workbook_auto};
    let columns = chunks
        .iter()
        .flat_map(|chunk| chunk.rows.iter().flat_map(|row| row.prs.keys()))
        .collect::<std::collections::BTreeSet<_>>();
    let headers = single_sheet::HEADERS
        .iter()
        .map(|value| (*value).to_owned())
        .chain(columns.iter().map(|value| (*value).clone()))
        .collect::<Vec<_>>();
    let count = chunks.iter().try_fold(0_usize, |count, chunk| {
        count
            .checked_add(chunk.rows.len())
            .ok_or_else(|| anyhow::anyhow!("prepared row count overflow"))
    })?;
    let mut workbook = open_workbook_auto(path)?;
    ensure!(
        workbook.sheet_names() == ["Athletes"],
        "verified export must contain exactly one Athletes sheet"
    );
    let range = workbook.worksheet_range("Athletes")?;
    ensure!(
        range.height() == count.saturating_add(1) && range.width() == headers.len(),
        "workbook dimensions differ from frozen prepared rows"
    );
    let actual_headers = range
        .rows()
        .next()
        .ok_or_else(|| anyhow::anyhow!("workbook header absent"))?
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    ensure!(
        actual_headers == headers,
        "workbook header differs from prepared column union"
    );
    chunks
        .iter()
        .flat_map(|chunk| &chunk.rows)
        .zip(range.rows().skip(1))
        .try_for_each(|(row, actual)| -> Result<()> {
            let expected = row
                .identity
                .iter()
                .cloned()
                .chain(columns.iter().map(|column| {
                    row.prs
                        .get(*column)
                        .map_or_else(String::new, |values| values.join("; "))
                }));
            ensure!(
                expected.eq(actual.iter().map(ToString::to_string)),
                "workbook cell differs from frozen prepared athlete row"
            );
            Ok(())
        })?;
    Ok((count, headers.len()))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChunk {
    pub athlete_ids: Vec<i64>,
    rows: Vec<single_sheet::Prepared>,
}

pub fn prepare_chunk(records: &[AthleteRecord], options: &ExportOptions) -> Result<PreparedChunk> {
    ensure!(
        records.len() <= 1000,
        "prepared chunk exceeds 1000 athletes"
    );
    let records = records
        .iter()
        .filter(|record| {
            selection::in_scope(record, options)
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
