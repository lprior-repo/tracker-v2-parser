mod sanitize;

use std::path::Path;

use anyhow::{Context, Result, ensure};
use calamine::{Reader, open_workbook_auto};
use rust_xlsxwriter::{Format, Workbook};
use serde::{Deserialize, Serialize};

use super::{ExportLayout, StagedExport, capacity};

pub const MAX_ROWS: usize = 10_000;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

pub const HEADERS: [&str; 18] = [
    "Checkpoint ID",
    "Source",
    "Kind",
    "State",
    "School",
    "School source ID",
    "Athlete",
    "Athlete source ID",
    "URL",
    "Status",
    "Attempts",
    "Durable state",
    "Retry after UTC",
    "Error",
    "Last success",
    "Created",
    "Next action",
    "AI guidance",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStatus {
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRow {
    pub checkpoint_id: i64,
    pub source: String,
    pub kind: String,
    pub state: String,
    pub school: String,
    pub school_source_id: String,
    pub athlete: String,
    pub athlete_source_id: String,
    pub url: String,
    pub status: ReviewStatus,
    pub attempts: i32,
    pub durable_state: String,
    pub retry_after: String,
    pub error: String,
    pub last_success: String,
    pub created: String,
    pub next_action: String,
    pub ai_guidance: String,
}

impl ReviewRow {
    pub(crate) fn sanitize(mut self) -> Result<Self> {
        self.url = sanitize::url(&self.url)?;
        self.error = sanitize::error(&self.error)?;
        self.next_action = match (self.status, self.durable_state.as_str()) {
            (ReviewStatus::Blocked, _) => "Human handling required; preserve the access stop.",
            (ReviewStatus::Failed, "deferred") => "Wait for automatic retry at the recorded time; preserve attempt counters and saved stops.",
            (ReviewStatus::Failed, "rejected") => "Investigate rejected checkpoint; fix the cause before an authorized retry.",
            (ReviewStatus::Failed, "exhausted") => "Investigate exhausted retry budget; preserve cumulative attempt counters.",
            (ReviewStatus::Failed, _) => "Investigate checkpoint and retained evidence before an authorized retry.",
        }.into();
        self.ai_guidance = if self.status == ReviewStatus::Failed && self.durable_state == "deferred" {
            "Wait for the automatic retry. Use public evidence and exact source IDs. Do not bypass login, paywalls, challenges or saved stops; do not infer missing results."
        } else {
            "Use public evidence and exact source IDs. Do not bypass login, paywalls, challenges or saved stops; do not infer missing results."
        }.into();
        ensure!(
            self.checkpoint_id > 0 && self.attempts >= 0,
            "invalid review checkpoint identity/attempt count"
        );
        ensure!(
            self.cells()
                .iter()
                .all(|cell| cell.encode_utf16().count() <= capacity::MAX_CELL_UNITS),
            "review cell exceeds Excel capacity"
        );
        Ok(self)
    }

    #[must_use]
    pub fn cells(&self) -> Vec<String> {
        vec![
            self.checkpoint_id.to_string(),
            self.source.clone(),
            self.kind.clone(),
            self.state.clone(),
            self.school.clone(),
            self.school_source_id.clone(),
            self.athlete.clone(),
            self.athlete_source_id.clone(),
            self.url.clone(),
            match self.status {
                ReviewStatus::Failed => "failed",
                ReviewStatus::Blocked => "blocked",
            }
            .into(),
            self.attempts.to_string(),
            self.durable_state.clone(),
            self.retry_after.clone(),
            self.error.clone(),
            self.last_success.clone(),
            self.created.clone(),
            self.next_action.clone(),
            self.ai_guidance.clone(),
        ]
    }
}

pub fn validate(rows: &[ReviewRow]) -> Result<()> {
    ensure!(
        rows.len() <= MAX_ROWS,
        "review exceeds 10000 checkpoint rows"
    );
    let bytes = rows.iter().try_fold(0_usize, |bytes, row| {
        let next = serde_json::to_vec(row)?.len();
        bytes
            .checked_add(next)
            .context("review storage byte counter overflow")
    })?;
    ensure!(bytes <= MAX_BYTES, "review exceeds 16 MiB storage capacity");
    ensure!(
        rows.windows(2)
            .all(|pair| pair.first().map(|row| row.checkpoint_id)
                < pair.get(1).map(|row| row.checkpoint_id)),
        "review checkpoint IDs must be strictly ascending"
    );
    ensure!(
        rows.iter().all(|row| row
            .cells()
            .iter()
            .all(|cell| cell.encode_utf16().count() <= capacity::MAX_CELL_UNITS)),
        "review cell exceeds Excel capacity"
    );
    Ok(())
}

pub(super) fn write(
    workbook: &mut Workbook,
    rows: &[ReviewRow],
    expected: &mut Vec<capacity::SheetCount>,
) -> Result<()> {
    validate(rows)?;
    let headers = HEADERS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    capacity::write(
        workbook,
        "Review",
        &headers,
        rows.iter().map(ReviewRow::cells),
        true,
        expected,
    )?;
    let sheet = workbook.worksheet_from_name("Review")?;
    sheet.set_default_row_height(90);
    let wrapped = Format::new().set_text_wrap();
    [8, 13, 16, 17].into_iter().try_for_each(|column| {
        sheet.set_column_format(column, &wrapped)?;
        Ok::<(), anyhow::Error>(())
    })?;
    sheet.set_column_width(4, 30)?;
    sheet.set_column_width(6, 25)?;
    sheet.set_column_width(8, 65)?;
    sheet.set_column_width(13, 65)?;
    sheet.set_column_width(16, 65)?;
    sheet.set_column_width(17, 70)?;
    Ok(())
}

pub fn verify(path: &Path, rows: &[ReviewRow]) -> Result<()> {
    validate(rows)?;
    let mut workbook = open_workbook_auto(path)?;
    let range = workbook.worksheet_range("Review")?;
    ensure!(
        range.width() == HEADERS.len() && range.height() == rows.len().saturating_add(1),
        "Review worksheet dimensions differ from frozen rows"
    );
    let actual_headers = range
        .rows()
        .next()
        .context("Review header absent")?
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    ensure!(
        actual_headers == HEADERS,
        "Review headers differ from frozen contract"
    );
    rows.iter()
        .zip(range.rows().skip(1))
        .try_for_each(|(row, actual)| {
            ensure!(
                row.cells()
                    .into_iter()
                    .eq(actual.iter().map(ToString::to_string)),
                "Review cell differs from frozen checkpoint row"
            );
            Ok(())
        })
}

impl StagedExport {
    pub fn with_review(mut self, rows: Vec<ReviewRow>) -> Result<Self> {
        ensure!(
            self.layout == ExportLayout::SingleSheet,
            "Review requires single-sheet athlete layout"
        );
        ensure!(self.review.is_none(), "Review rows have already been bound");
        validate(&rows)?;
        self.review = Some(rows);
        Ok(self)
    }
}
