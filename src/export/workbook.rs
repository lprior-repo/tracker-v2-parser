use super::{ExportIssue, ExportOptions, capacity, rows, selection};
use crate::model::AthleteRecord;
use anyhow::{Context, Result};
use rust_xlsxwriter::Workbook;
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
    let mut expected = Vec::new();
    write_athletes(&mut workbook, &eligible, &mut expected)?;
    write_issues(&mut workbook, records, issues, &mut expected)?;
    super::profiles::write(&mut workbook, records, &mut expected)?;
    if options.per_athlete_sheets {
        eligible
            .iter()
            .try_for_each(|record| write_detail(&mut workbook, record, &mut expected))?;
    }
    workbook.save(path).context("saving temporary XLSX")?;
    capacity::verify(path, &expected)
}

fn write_athletes(
    workbook: &mut Workbook,
    records: &[&AthleteRecord],
    expected: &mut Vec<capacity::SheetCount>,
) -> Result<()> {
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
    capacity::write(
        workbook,
        "Athletes",
        &headers,
        records
            .iter()
            .zip(&prepared)
            .map(|(record, prepared)| rows::cells(record, prepared, &columns)),
        true,
        expected,
    )
}

fn write_issues(
    workbook: &mut Workbook,
    records: &[&AthleteRecord],
    coverage: &[ExportIssue],
    expected: &mut Vec<capacity::SheetCount>,
) -> Result<()> {
    let headers = ["athlete_id", "name", "source_profiles", "issues"].map(str::to_owned);
    let record_rows = records.iter().filter_map(|record| {
        let issues = selection::issues(record);
        (!issues.is_empty()).then(|| {
            vec![
                record.id.to_string(),
                record.name.clone(),
                record.profiles.join("; "),
                issues.join("; "),
            ]
        })
    });
    let coverage_rows = coverage.iter().map(|issue| {
        vec![
            String::new(),
            issue.source.clone(),
            issue.url.clone(),
            issue.detail.clone(),
        ]
    });
    capacity::write(
        workbook,
        "Issues",
        &headers,
        record_rows.chain(coverage_rows),
        false,
        expected,
    )
}

fn write_detail(
    workbook: &mut Workbook,
    record: &AthleteRecord,
    expected: &mut Vec<capacity::SheetCount>,
) -> Result<()> {
    let headers = [
        "event",
        "raw_mark",
        "sport",
        "source",
        "source_url",
        "date",
        "course",
        "comparison_context",
    ]
    .map(str::to_owned);
    let rows = record.performances.iter().map(|evidence| {
        let performance = &evidence.performance;
        let context = crate::marks::comparable(evidence).map_or_else(
            || "not comparable; retained as published".to_owned(),
            |mark| mark.column,
        );
        vec![
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
        ]
    });
    capacity::write(
        workbook,
        &format!("Athlete_{}", record.id),
        &headers,
        rows,
        false,
        expected,
    )
}
