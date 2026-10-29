use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use rust_xlsxwriter::Workbook;

#[path = "single_sheet_prs.rs"]
mod prs;

#[path = "single_sheet_identity.rs"]
mod identity;

use super::{ReviewRow, capacity, review};
use crate::model::AthleteRecord;

pub(super) const HEADERS: [&str; 14] = [
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
    "Coach",
    "Coach email",
];

#[path = "single_sheet_spool.rs"]
mod spool;

pub(super) use spool::Accumulator;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Prepared {
    pub(super) identity: Vec<String>,
    pub(super) prs: BTreeMap<String, Vec<String>>,
}

impl Prepared {
    fn retained_bytes(&self) -> Result<usize> {
        let identity = self
            .identity
            .capacity()
            .checked_mul(std::mem::size_of::<String>())
            .context("prepared identity size overflow")?;
        let base = self
            .prs
            .len()
            .checked_mul(1024)
            .and_then(|bytes| bytes.checked_add(identity))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()))
            .context("prepared PR size overflow")?;
        let mut strings = self.identity.iter().chain(
            self.prs
                .iter()
                .flat_map(|(column, values)| std::iter::once(column).chain(values)),
        );
        let bytes = strings.try_fold(base, |bytes, value| {
            bytes
                .checked_add(value.capacity())
                .context("prepared string size overflow")
        })?;
        self.prs.values().try_fold(bytes, |bytes, values| {
            values
                .capacity()
                .checked_mul(std::mem::size_of::<String>())
                .and_then(|size| bytes.checked_add(size))
                .context("prepared values size overflow")
        })
    }
}

pub(super) fn prepare(record: &AthleteRecord) -> Result<Prepared> {
    Ok(Prepared {
        identity: identity::identity(record),
        prs: prs::prepare(record)?,
    })
}

pub(super) fn headers(columns: &BTreeSet<String>) -> Vec<String> {
    HEADERS
        .iter()
        .map(|value| (*value).to_owned())
        .chain(columns.iter().cloned())
        .collect()
}

pub(super) fn cells(record: Prepared, columns: &BTreeSet<String>) -> Result<Vec<String>> {
    let values = record
        .identity
        .into_iter()
        .chain(columns.iter().map(|column| {
            record
                .prs
                .get(column)
                .map_or_else(String::new, |values| values.join("; "))
        }))
        .collect::<Vec<_>>();
    ensure!(
        values
            .iter()
            .all(|value| value.encode_utf16().count() <= capacity::MAX_CELL_UNITS),
        "single-sheet cell exceeds Excel capacity; select a narrower scope"
    );
    Ok(values)
}

fn write_prepared(
    path: &Path,
    columns: &BTreeSet<String>,
    records: impl Iterator<Item = Result<Prepared>>,
    review: Option<&[ReviewRow]>,
) -> Result<()> {
    let headers = headers(columns);
    let mut workbook = Workbook::new();
    let mut expected = Vec::new();
    let values = records.map(|record| cells(record?, columns));
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
    if let Some(rows) = review {
        review::write(&mut workbook, rows, &mut expected)?;
        ensure!(
            expected.len() == 2,
            "review export must contain exactly two worksheets"
        );
    }
    workbook.save(path).context("saving single-sheet XLSX")?;
    drop(workbook);
    capacity::verify(path, &expected)
}
