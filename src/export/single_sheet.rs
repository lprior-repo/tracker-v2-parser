use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use rust_xlsxwriter::Workbook;

use super::{capacity, rows};
use crate::model::{AthleteRecord, Coach, EvidencedPerformance, Sport};

const HEADERS: [&str; 12] = [
    "Name",
    "Gender",
    "Graduation year",
    "School",
    "School address",
    "Track coach",
    "Track coach email",
    "XC coach",
    "XC coach email",
    "School email",
    "Profile",
    "Athlete ID",
];

type PublishedPrs = BTreeMap<String, Vec<String>>;

pub(super) fn write_and_verify(path: &Path, records: &[&AthleteRecord]) -> Result<()> {
    let prs = records
        .iter()
        .map(|record| published_prs(record))
        .collect::<Vec<_>>();
    let columns = prs.iter().flat_map(BTreeMap::keys).collect::<BTreeSet<_>>();
    ensure!(
        columns.len().saturating_add(HEADERS.len()) <= 16_384,
        "single-sheet export exceeds Excel column capacity; select a narrower scope"
    );
    let headers = HEADERS
        .iter()
        .map(|s| (*s).to_owned())
        .chain(columns.iter().map(|s| (*s).clone()))
        .collect::<Vec<_>>();
    let mut workbook = Workbook::new();
    let mut expected = Vec::new();
    let values = records.iter().zip(&prs).map(|(record, prs)| {
        let cells = identity(record)
            .into_iter()
            .chain(columns.iter().map(|column| {
                prs.get(*column)
                    .map_or_else(String::new, |values| values.join("; "))
            }))
            .collect::<Vec<_>>();
        ensure!(
            cells
                .iter()
                .all(|s| s.encode_utf16().count() <= capacity::MAX_CELL_UNITS),
            "single-sheet cell exceeds Excel capacity; select a narrower scope"
        );
        Ok(cells)
    });
    capacity::write_fallible(
        &mut workbook,
        "Athletes",
        &headers,
        values,
        true,
        &mut expected,
    )?;
    ensure!(
        expected.len() == 1,
        "single-sheet export exceeds worksheet capacity"
    );
    let sheet = workbook.worksheet_from_name("Athletes")?;
    sheet.set_row_height(0, 64)?;
    sheet.set_column_width(0, 25)?;
    sheet.set_column_width(4, 44)?;
    sheet.set_column_width(6, 38)?;
    sheet.set_column_width(8, 38)?;
    sheet.set_column_width(10, 48)?;
    workbook.save(path).context("saving single-sheet XLSX")?;
    drop(workbook);
    capacity::verify(path, &expected)
}

fn published_prs(record: &AthleteRecord) -> PublishedPrs {
    record
        .performances
        .iter()
        .filter(|evidence| evidence.performance.is_personal_record)
        .fold(BTreeMap::new(), |mut columns, evidence| {
            let row = &evidence.performance;
            let date = row
                .date
                .as_deref()
                .map_or(String::new(), |date| format!(" ({date})"));
            columns
                .entry(pr_column(evidence))
                .or_insert_with(Vec::new)
                .push(format!("{}{date}", row.mark));
            columns
        })
        .into_iter()
        .map(|(key, mut values)| {
            values.sort_unstable();
            (key, values)
        })
        .collect()
}

fn pr_column(evidence: &EvidencedPerformance) -> String {
    let row = &evidence.performance;
    let sport = match row.sport {
        Sport::Indoor => "Indoor",
        Sport::Outdoor => "Outdoor",
        Sport::CrossCountry => "XC",
    };
    let level = row
        .source_values
        .as_ref()
        .and_then(|v| v.get("level"))
        .and_then(serde_json::Value::as_str)
        .map_or("unspecified", |level| level);
    let level = if level == "all" { "all levels" } else { level };
    format!(
        "{sport} {} PR [{level}; {}]",
        row.event,
        evidence.source.as_str()
    )
}

fn identity(record: &AthleteRecord) -> Vec<String> {
    vec![
        record.name.clone(),
        record
            .gender
            .as_deref()
            .map_or_else(String::new, str::to_owned),
        record
            .graduation_year
            .map_or_else(String::new, |year| year.to_string()),
        rows::joined(record.schools.iter().map(|school| school.name.as_str())),
        rows::addresses(record),
        coach_field(record, Program::Track, CoachField::Name),
        coach_field(record, Program::Track, CoachField::Email),
        coach_field(record, Program::CrossCountry, CoachField::Name),
        coach_field(record, Program::CrossCountry, CoachField::Email),
        rows::joined(
            record
                .schools
                .iter()
                .filter_map(|school| school.public_email.as_deref()),
        ),
        rows::joined(record.profiles.iter().map(String::as_str)),
        record.id.to_string(),
    ]
}

#[derive(Clone, Copy)]
enum Program {
    Track,
    CrossCountry,
}

#[derive(Clone, Copy)]
enum CoachField {
    Name,
    Email,
}

fn coach_field(record: &AthleteRecord, program: Program, field: CoachField) -> String {
    rows::joined(
        record
            .coaches
            .iter()
            .filter(|coach| matches_program(coach, program))
            .filter_map(|coach| match field {
                CoachField::Name => coach.name.as_deref(),
                CoachField::Email => coach.email.as_deref(),
            }),
    )
}

fn matches_program(coach: &Coach, program: Program) -> bool {
    match program {
        Program::Track => coach.sport != Some(Sport::CrossCountry),
        Program::CrossCountry => coach.sport == Some(Sport::CrossCountry),
    }
}
