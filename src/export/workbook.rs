use super::{ExportIssue, ExportOptions, capacity, rows, selection};
use crate::model::{AthleteRecord, EvidencedPerformance};
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
    let mut workbook = Workbook::new();
    let mut expected = Vec::new();
    write_athletes(&mut workbook, records, &mut expected)?;
    write_issues(&mut workbook, records, issues, &mut expected)?;
    write_observations(&mut workbook, "Observations", records, &mut expected)?;
    super::profiles::write(&mut workbook, records, &mut expected)?;
    if options.per_athlete_sheets {
        records.iter().try_for_each(|record| {
            write_observations(
                &mut workbook,
                &format!("Athlete_{}", record.id),
                &[*record],
                &mut expected,
            )
        })?;
    }
    capacity::write_details(&mut workbook, &mut expected)?;
    workbook.save(path).context("saving temporary XLSX")?;
    drop(workbook);
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
        .flat_map(|row| row.bests.keys())
        .filter(|key| key.encode_utf16().count().saturating_add(12) <= capacity::MAX_CELL_UNITS)
        .fold(BTreeSet::new(), |mut columns, key| {
            columns.insert(key.as_str());
            if columns.len() > capacity::MAX_PR_KEYS {
                let _largest = columns.pop_last();
            }
            columns
        })
        .into_iter()
        .map(str::to_owned)
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
    )?;
    let headers = [
        "athlete_id",
        "comparison_context",
        "raw_mark",
        "source_url",
        "raw_event",
    ]
    .map(str::to_owned);
    let overflow = records
        .iter()
        .zip(&prepared)
        .flat_map(|(record, prepared)| {
            let columns = &columns;
            prepared
                .bests
                .iter()
                .filter(move |(key, _)| !columns.contains(*key))
                .map(move |(key, (_, evidence))| {
                    vec![
                        record.id.to_string(),
                        key.clone(),
                        evidence.performance.mark.clone(),
                        evidence.url.clone(),
                        evidence.performance.event.clone(),
                    ]
                })
        });
    capacity::write(workbook, "PR_overflow", &headers, overflow, false, expected)
}

fn write_issues(
    workbook: &mut Workbook,
    records: &[&AthleteRecord],
    coverage: &[ExportIssue],
    expected: &mut Vec<capacity::SheetCount>,
) -> Result<()> {
    let headers = [
        "athlete_id",
        "name",
        "source_profiles",
        "issues",
        "issue_id",
        "part",
        "parts",
    ]
    .map(str::to_owned);
    let summaries = records.iter().filter_map(|record| {
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
    let contacts = records.iter().flat_map(|record| {
        selection::contact_issues(record)
            .map(move |note| vec![record.id.to_string(), String::new(), String::new(), note])
    });
    let rejected = records.iter().flat_map(|record| {
        record.performances.iter().filter_map(move |evidence| {
            selection::hs_comparison(evidence).err().map(|reason| {
                rejection(evidence, reason).map(|detail| {
                    vec![
                        record.id.to_string(),
                        String::new(),
                        evidence.url.clone(),
                        detail,
                    ]
                })
            })
        })
    });
    let coverage = coverage.iter().map(|issue| {
        vec![
            String::new(),
            issue.source.clone(),
            issue.url.clone(),
            issue.detail.clone(),
        ]
    });
    let rows = summaries
        .chain(contacts)
        .map(Ok)
        .chain(rejected)
        .chain(coverage.map(Ok))
        .enumerate()
        .flat_map(|(index, row)| issue_parts(index, row));
    capacity::write_fallible(workbook, "Issues", &headers, rows, false, expected)
}

fn issue_parts(index: usize, row: Result<Vec<String>>) -> Vec<Result<Vec<String>>> {
    let mut cells = match row {
        Ok(cells) => cells,
        Err(error) => return vec![Err(error)],
    };
    let Some(text) = cells.pop() else {
        return vec![Err(anyhow::anyhow!("missing issue text"))];
    };
    let id = cells.first().map_or_else(String::new, Clone::clone);
    let chunks = if text.is_empty() {
        vec![""]
    } else {
        capacity::chunks(&text).collect::<Vec<_>>()
    };
    chunks
        .iter()
        .enumerate()
        .map(|(part, chunk)| {
            let mut row = if part == 0 {
                std::mem::take(&mut cells)
            } else {
                vec![id.clone(), String::new(), String::new()]
            };
            row.extend([
                (*chunk).to_owned(),
                index.saturating_add(1).to_string(),
                part.saturating_add(1).to_string(),
                chunks.len().to_string(),
            ]);
            Ok(row)
        })
        .collect()
}

fn rejection(evidence: &EvidencedPerformance, reason: &str) -> Result<String> {
    let observation = crate::transport::redact_public_json(&serde_json::to_value(evidence)?)?;
    Ok(format!(
        "excluded from HS PR ({reason}); source={}; level={}; event={}; mark={}; observation={observation}",
        evidence.source.as_str(),
        level(evidence),
        evidence.performance.event,
        evidence.performance.mark,
    ))
}

fn level(evidence: &EvidencedPerformance) -> &str {
    evidence
        .performance
        .source_values
        .as_ref()
        .and_then(|values| values.get("level"))
        .and_then(serde_json::Value::as_str)
        .map_or("unknown", core::convert::identity)
}

fn write_observations(
    workbook: &mut Workbook,
    name: &str,
    records: &[&AthleteRecord],
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
        "reported_status",
        "athlete_id",
        "published_level",
        "hs_pr_eligibility",
        "published_observation",
    ]
    .map(str::to_owned);
    let rows = records.iter().flat_map(|record| {
        record.performances.iter().map(move |evidence| {
            let performance = &evidence.performance;
            let context = crate::marks::comparison(evidence).map_or_else(
                |reason| format!("not comparable ({reason}); retained as published"),
                |mark| mark.column,
            );
            let eligibility = selection::hs_comparison(evidence)
                .map_or_else(str::to_owned, |_| "eligible".to_owned());
            let observation =
                crate::transport::redact_public_json(&serde_json::to_value(evidence)?)?;
            Ok(vec![
                performance.event.clone(),
                performance.mark.clone(),
                performance.sport.as_str().to_owned(),
                evidence.source.as_str().to_owned(),
                evidence.url.clone(),
                optional(performance.date.as_deref()),
                optional(performance.course.as_deref()),
                context,
                optional(performance.reported_status.as_deref()),
                record.id.to_string(),
                level(evidence).to_owned(),
                eligibility,
                observation,
            ])
        })
    });
    capacity::write_fallible(workbook, name, &headers, rows, false, expected)
}

fn optional(value: Option<&str>) -> String {
    value.map_or_else(String::new, str::to_owned)
}
