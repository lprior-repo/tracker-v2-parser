use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use anyhow::{Context, Result};

use super::{ExportIssue, ExportOptions, TEMP_SEQUENCE, workbook};
use crate::model::AthleteRecord;

pub(super) fn detailed(
    path: &Path,
    records: &[&AthleteRecord],
    issues: &[ExportIssue],
    options: &ExportOptions,
    staged: &mut Vec<PathBuf>,
) -> Result<()> {
    with_writer(path, staged, |temporary| {
        workbook::write_and_verify(temporary, records, issues, options)
    })
}

pub(super) fn with_writer(
    path: &Path,
    staged: &mut Vec<PathBuf>,
    write: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let name = path
        .file_name()
        .context("workbook path needs a file name")?
        .to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    staged.push(temporary);
    let temporary = staged.last().context("missing staged workbook")?;
    write(temporary)?;
    file.sync_all().context("syncing verified workbook")
}
