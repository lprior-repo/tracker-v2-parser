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

use super::capacity;
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

const MAX_PREPARED_BYTES: usize = 128 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
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

#[derive(Default)]
pub(super) struct Accumulator {
    rows: Vec<Prepared>,
    bytes: usize,
    failed: bool,
}

impl Accumulator {
    pub(super) fn append(&mut self, records: &[&AthleteRecord], row_limit: usize) -> Result<()> {
        ensure!(!self.failed, "cannot continue a failed single-sheet export");
        let result = records
            .iter()
            .try_for_each(|record| self.append_prepared(vec![prepare(record)?], row_limit));
        self.failed = result.is_err();
        result
    }

    pub(super) fn append_prepared(&mut self, rows: Vec<Prepared>, row_limit: usize) -> Result<()> {
        ensure!(!self.failed, "cannot continue a failed single-sheet export");
        self.failed = true;
        let result = rows.into_iter().try_for_each(|row| {
            ensure!(self.rows.len() < row_limit, "single-sheet export exceeds configured row capacity; increase rows_per_file within Excel capacity");
            ensure!(row.identity.len() == HEADERS.len(), "prepared identity column count mismatch");
            let bytes = self.bytes.checked_add(row.retained_bytes()?).context("single-sheet storage budget overflow")?;
            ensure!(bytes <= MAX_PREPARED_BYTES, "single-sheet export exceeds 128 MiB prepared-storage capacity; no complete export was published");
            self.rows.push(row);
            self.bytes = bytes;
            Ok(())
        });
        self.failed = result.is_err();
        result
    }

    pub(super) fn write_and_verify(&self, path: &Path) -> Result<()> {
        ensure!(!self.failed, "cannot publish a failed single-sheet export");
        write_prepared(path, &self.rows)
    }
}

pub(super) fn prepare(record: &AthleteRecord) -> Result<Prepared> {
    Ok(Prepared {
        identity: identity::identity(record),
        prs: prs::prepare(record)?,
    })
}

fn write_prepared(path: &Path, records: &[Prepared]) -> Result<()> {
    let columns = records
        .iter()
        .flat_map(|row| row.prs.keys())
        .collect::<BTreeSet<_>>();
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
    let values = records.iter().map(|record| {
        let cells = record
            .identity
            .iter()
            .cloned()
            .chain(columns.iter().map(|column| {
                record
                    .prs
                    .get(*column)
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
