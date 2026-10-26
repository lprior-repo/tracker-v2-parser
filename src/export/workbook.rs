use super::{ExportIssue, ExportOptions, rows, selection};
use crate::model::AthleteRecord;
use anyhow::{Context, Result};
use calamine::{Reader, Xlsx, open_workbook};
use rust_xlsxwriter::{Color, Format, Url, Workbook, Worksheet};
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn write_and_verify(
    path: &Path,
    records: &[&AthleteRecord],
    issues: &[ExportIssue],
    options: &ExportOptions,
) -> Result<()> {
    let eligible: Vec<_> = records
        .iter()
        .copied()
        .filter(|record| selection::eligible(record, options))
        .collect();
    let mut workbook = Workbook::new();
    write_athletes(workbook.add_worksheet(), &eligible)?;
    write_issues(workbook.add_worksheet(), records, issues)?;
    if options.per_athlete_sheets {
        eligible
            .iter()
            .try_for_each(|record| write_detail(workbook.add_worksheet(), record))?;
    }
    workbook.save(path).context("saving temporary XLSX")?;
    verify(
        path,
        eligible.len(),
        issue_count(records)
            .checked_add(issues.len())
            .context("issue count overflow")?,
    )
}

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0x17_36_5D))
        .set_font_color(Color::White)
        .set_text_wrap()
}

fn write_headers(sheet: &mut Worksheet, headers: &[String]) -> Result<()> {
    anyhow::ensure!(
        headers.len() <= 16_384,
        "too many comparable event columns for Excel"
    );
    let format = header_format();
    headers.iter().enumerate().try_for_each(|(column, text)| {
        sheet.write_string_with_format(0, u16::try_from(column)?, text, &format)?;
        Ok::<(), anyhow::Error>(())
    })?;
    sheet.set_freeze_panes(1, 0)?;
    sheet.set_row_height(0, 32)?;
    if let Some(last) = headers.len().checked_sub(1) {
        sheet.set_column_range_width(0, u16::try_from(last)?, 24)?;
    }
    Ok(())
}

fn write_cells(sheet: &mut Worksheet, row: usize, cells: &[String]) -> Result<()> {
    let row = u32::try_from(row)?;
    anyhow::ensure!(row < 1_048_576, "worksheet exceeds Excel row limit");
    cells.iter().enumerate().try_for_each(|(column, text)| {
        let column = u16::try_from(column)?;
        if (text.starts_with("https://") || text.starts_with("http://")) && !text.contains("; ") {
            sheet.write_url(row, column, Url::new(text.as_str()))?;
        } else {
            sheet.write_string(row, column, text)?;
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn write_athletes(sheet: &mut Worksheet, records: &[&AthleteRecord]) -> Result<()> {
    sheet.set_name("Athletes")?;
    let prepared: Vec<_> = records.iter().map(|record| rows::prepare(record)).collect();
    let columns: BTreeSet<_> = prepared
        .iter()
        .flat_map(|row| row.bests.keys().cloned())
        .collect();
    let headers = rows::BASE_HEADERS
        .iter()
        .map(|value| (*value).to_owned())
        .chain(columns.iter().flat_map(|key| {
            [
                format!("{key}::mark"),
                format!("{key}::source_url"),
                format!("{key}::raw_event"),
            ]
        }))
        .collect::<Vec<_>>();
    write_headers(sheet, &headers)?;
    records
        .iter()
        .zip(&prepared)
        .enumerate()
        .try_for_each(|(index, (record, prepared))| {
            write_cells(
                sheet,
                index.checked_add(1).context("worksheet row overflow")?,
                &rows::cells(record, prepared, &columns),
            )
        })?;
    if let Some(last) = headers.len().checked_sub(1) {
        sheet.autofilter(0, 0, u32::try_from(records.len())?, u16::try_from(last)?)?;
    }
    Ok(())
}

fn write_issues(
    sheet: &mut Worksheet,
    records: &[&AthleteRecord],
    coverage: &[ExportIssue],
) -> Result<()> {
    sheet.set_name("Issues")?;
    write_headers(
        sheet,
        &[
            "athlete_id".to_owned(),
            "name".to_owned(),
            "source_profiles".to_owned(),
            "issues".to_owned(),
        ],
    )?;
    records
        .iter()
        .filter_map(|record| {
            let issues = selection::issues(record);
            (!issues.is_empty()).then_some((record, issues))
        })
        .enumerate()
        .try_for_each(|(index, (record, issues))| {
            write_cells(
                sheet,
                index.checked_add(1).context("issue row overflow")?,
                &[
                    record.id.to_string(),
                    record.name.clone(),
                    record.profiles.join("; "),
                    issues.join("; "),
                ],
            )
        })?;
    let offset = issue_count(records)
        .checked_add(1)
        .context("issue row overflow")?;
    coverage.iter().enumerate().try_for_each(|(index, issue)| {
        write_cells(
            sheet,
            offset.checked_add(index).context("issue row overflow")?,
            &[
                String::new(),
                issue.source.clone(),
                issue.url.clone(),
                issue.detail.clone(),
            ],
        )
    })
}

fn write_detail(sheet: &mut Worksheet, record: &AthleteRecord) -> Result<()> {
    sheet.set_name(format!("Athlete_{}", record.id))?;
    write_headers(
        sheet,
        &[
            "event".to_owned(),
            "raw_mark".to_owned(),
            "sport".to_owned(),
            "source".to_owned(),
            "source_url".to_owned(),
            "date".to_owned(),
            "course".to_owned(),
            "comparison_context".to_owned(),
        ],
    )?;
    record
        .performances
        .iter()
        .enumerate()
        .try_for_each(|(index, evidence)| {
            let performance = &evidence.performance;
            let context = crate::marks::comparable(evidence).map_or_else(
                || "not comparable; retained as published".to_owned(),
                |mark| mark.column,
            );
            write_cells(
                sheet,
                index
                    .checked_add(1)
                    .context("athlete detail row overflow")?,
                &[
                    performance.event.clone(),
                    performance.mark.clone(),
                    performance.sport.as_str().to_owned(),
                    evidence.source.as_str().to_owned(),
                    evidence.url.clone(),
                    performance
                        .date
                        .clone()
                        .map_or(String::new(), core::convert::identity),
                    performance
                        .course
                        .clone()
                        .map_or(String::new(), core::convert::identity),
                    context,
                ],
            )
        })
}

fn issue_count(records: &[&AthleteRecord]) -> usize {
    records
        .iter()
        .filter(|record| !selection::issues(record).is_empty())
        .count()
}

fn verify(path: &Path, expected_athletes: usize, expected_issues: usize) -> Result<()> {
    let mut workbook: Xlsx<_> =
        open_workbook(path).context("reopening temporary XLSX for verification")?;
    let athletes = workbook
        .worksheet_range("Athletes")
        .context("verifying Athletes sheet")?;
    let issues = workbook
        .worksheet_range("Issues")
        .context("verifying Issues sheet")?;
    anyhow::ensure!(
        athletes.height()
            == expected_athletes
                .checked_add(1)
                .context("athlete count overflow")?,
        "XLSX athlete row count mismatch"
    );
    anyhow::ensure!(
        issues.height()
            == expected_issues
                .checked_add(1)
                .context("issue count overflow")?,
        "XLSX issue row count mismatch"
    );
    Ok(())
}
