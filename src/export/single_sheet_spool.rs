use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};

use super::{HEADERS, Prepared, ReviewRow, prepare};
use crate::model::AthleteRecord;

const MAX_BATCH_BYTES: usize = 128 * 1024 * 1024;
const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;
const MAX_DISK_BYTES: usize = 512 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Spool {
    path: PathBuf,
    file: Option<File>,
}

impl Spool {
    fn new(output: &Path) -> Result<Self> {
        let name = output
            .file_name()
            .context("prepared spool output name absent")?
            .to_string_lossy();
        let path = output.with_file_name(format!(
            ".{name}.{}-{}.prepared.jsonl",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .context("creating private prepared-row spool")?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Err(error) = std::fs::remove_file(&self.path) {
            tracing::warn!(%error, "failed to remove owned prepared-row spool");
        }
    }
}

pub struct Accumulator {
    output: PathBuf,
    spool: Option<Spool>,
    columns: BTreeSet<String>,
    row_count: usize,
    disk_bytes: usize,
    header_bytes: usize,
    failed: bool,
}

impl Accumulator {
    pub fn new(output: &Path) -> Self {
        Self {
            output: output.into(),
            spool: None,
            columns: BTreeSet::new(),
            row_count: 0,
            disk_bytes: 0,
            header_bytes: 0,
            failed: false,
        }
    }

    pub fn append(&mut self, records: &[&AthleteRecord], row_limit: usize) -> Result<()> {
        ensure!(!self.failed, "cannot continue a failed single-sheet export");
        let result = records
            .iter()
            .try_for_each(|record| self.append_prepared(vec![prepare(record)?], row_limit));
        self.failed = result.is_err();
        if self.failed {
            self.spool = None;
        }
        result
    }

    pub fn append_prepared(&mut self, rows: Vec<Prepared>, row_limit: usize) -> Result<()> {
        ensure!(!self.failed, "cannot continue a failed single-sheet export");
        self.failed = true;
        let result = (|| {
            ensure!(rows.len() <= 1000, "prepared batch exceeds 1000 rows");
            let retained = rows.iter().try_fold(0_usize, |total, row| {
                total
                    .checked_add(row.retained_bytes()?)
                    .context("prepared batch size overflow")
            })?;
            ensure!(
                retained <= MAX_BATCH_BYTES,
                "single-sheet prepared batch exceeds 128 MiB memory capacity"
            );
            rows.into_iter()
                .try_for_each(|row| self.store(&row, row_limit))
        })();
        self.failed = result.is_err();
        if self.failed {
            self.spool = None;
        }
        result
    }

    fn store(&mut self, row: &Prepared, row_limit: usize) -> Result<()> {
        ensure!(
            self.row_count < row_limit,
            "single-sheet export exceeds configured row capacity; increase rows_per_file within Excel capacity"
        );
        ensure!(
            row.identity.len() == HEADERS.len(),
            "prepared identity column count mismatch"
        );
        row.prs.keys().try_for_each(|column| -> Result<()> {
            if self.columns.insert(column.clone()) {
                self.header_bytes = self
                    .header_bytes
                    .checked_add(column.len())
                    .context("prepared header size overflow")?;
                ensure!(
                    self.header_bytes <= MAX_HEADER_BYTES,
                    "single-sheet headers exceed 16 MiB capacity"
                );
                ensure!(
                    self.columns.len().saturating_add(HEADERS.len()) <= 16_384,
                    "single-sheet export exceeds Excel column capacity; select a narrower scope"
                );
            }
            Ok(())
        })?;
        let encoded = serde_json::to_vec(row)?;
        ensure!(
            encoded.len() < MAX_ROW_BYTES,
            "single-sheet prepared row exceeds 8 MiB capacity"
        );
        let size = self
            .disk_bytes
            .checked_add(encoded.len())
            .and_then(|bytes| bytes.checked_add(1))
            .context("prepared spool size overflow")?;
        ensure!(
            size <= MAX_DISK_BYTES,
            "single-sheet prepared spool exceeds 512 MiB disk capacity"
        );
        if self.spool.is_none() {
            self.spool = Some(Spool::new(&self.output)?);
        }
        let spool = self.spool.as_mut().context("prepared spool absent")?;
        let file = spool.file.as_mut().context("prepared spool file closed")?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        self.disk_bytes = size;
        self.row_count = self
            .row_count
            .checked_add(1)
            .context("prepared row count overflow")?;
        Ok(())
    }

    fn rows(&self) -> Result<impl Iterator<Item = Result<Prepared>>> {
        let mut reader = self
            .spool
            .as_ref()
            .map(|spool| File::open(&spool.path).map(BufReader::new))
            .transpose()?;
        let mut remaining = self.row_count;
        let mut ended = false;
        Ok(std::iter::from_fn(move || {
            if ended {
                return None;
            }
            let input = reader.as_mut()?;
            let result = (|| {
                let mut line = Vec::new();
                let size = input
                    .take(u64::try_from(MAX_ROW_BYTES)?.saturating_add(1))
                    .read_until(b'\n', &mut line)?;
                if size == 0 {
                    ensure!(remaining == 0, "prepared spool ended before recorded rows");
                    return Ok(None);
                }
                ensure!(
                    size <= MAX_ROW_BYTES && line.last() == Some(&b'\n'),
                    "prepared spool row exceeds bound or is truncated"
                );
                remaining = remaining
                    .checked_sub(1)
                    .context("prepared spool contains unrecorded rows")?;
                Ok(Some(serde_json::from_slice(&line)?))
            })();
            match result {
                Ok(Some(row)) => Some(Ok(row)),
                Ok(None) => {
                    ended = true;
                    None
                }
                Err(error) => {
                    ended = true;
                    Some(Err(error))
                }
            }
        }))
    }

    pub fn write_and_verify(&self, path: &Path, review: Option<&[ReviewRow]>) -> Result<()> {
        ensure!(!self.failed, "cannot publish a failed single-sheet export");
        super::write_prepared(path, &self.columns, self.rows()?, review)?;
        let rows = self.rows()?.map(|row| super::cells(row?, &self.columns));
        super::super::prepared_verify::verify_rows(
            path,
            &super::headers(&self.columns),
            rows,
            self.row_count,
            review,
        )?;
        Ok(())
    }
}
