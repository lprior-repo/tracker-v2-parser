mod rows;
mod selection;
mod workbook;

use crate::model::AthleteRecord;
use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub states: Vec<String>,
    pub graduation_years: Vec<i32>,
    pub rows_per_file: usize,
    pub per_athlete_sheets: bool,
}

#[derive(Debug, Clone)]
pub struct ExportIssue {
    pub source: String,
    pub url: String,
    pub detail: String,
}

pub fn write_spreadsheets_with_issues(
    path: &Path,
    records: &[AthleteRecord],
    issues: &[ExportIssue],
    options: &ExportOptions,
) -> Result<Vec<PathBuf>> {
    write_parts(path, records, issues, options)
}

pub fn write_spreadsheets(
    path: &Path,
    records: &[AthleteRecord],
    options: &ExportOptions,
) -> Result<Vec<PathBuf>> {
    write_parts(path, records, &[], options)
}

fn write_parts(
    path: &Path,
    records: &[AthleteRecord],
    issues: &[ExportIssue],
    options: &ExportOptions,
) -> Result<Vec<PathBuf>> {
    anyhow::ensure!(
        !options.states.is_empty() && !options.graduation_years.is_empty(),
        "export requires explicit states and graduation years"
    );
    anyhow::ensure!(
        (1..=1_048_575).contains(&options.rows_per_file),
        "rows_per_file must be 1..=1048575"
    );
    anyhow::ensure!(
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx")),
        "export path must end in .xlsx"
    );
    anyhow::ensure!(
        !path.try_exists()?,
        "output already exists: {}",
        path.display()
    );
    let selected: Vec<_> = records
        .iter()
        .filter(|record| selection::in_scope(record, options))
        .collect();
    let split = selected.len() > options.rows_per_file;
    let destinations = if selected.is_empty() {
        vec![path.to_path_buf()]
    } else {
        selected
            .chunks(options.rows_per_file)
            .enumerate()
            .map(|(index, _)| destination(path, index, split))
            .collect::<Result<Vec<_>>>()?
    };
    destinations.iter().try_for_each(|destination| {
        anyhow::ensure!(
            !destination.try_exists()?,
            "output already exists: {}",
            destination.display()
        );
        Ok::<(), anyhow::Error>(())
    })?;
    let mut staged = Vec::with_capacity(destinations.len());
    let staging = if selected.is_empty() {
        stage(path, &[], issues, options, &mut staged)
    } else {
        selected
            .chunks(options.rows_per_file)
            .zip(&destinations)
            .try_for_each(|(chunk, destination)| {
                stage(destination, chunk, issues, options, &mut staged)
            })
    };
    let result = staging.and_then(|()| publish_parts(&staged, &destinations));
    let cleanup = cleanup_staged(&staged);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(destinations),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.context(
            "all workbook parts were published, but temporary-file cleanup failed",
        )),
        (Err(error), Err(cleanup)) => Err(error.context(cleanup.to_string())),
    }
}

fn destination(path: &Path, index: usize, split: bool) -> Result<PathBuf> {
    if !split {
        return Ok(path.to_path_buf());
    }
    let stem = path
        .file_stem()
        .context("workbook path requires a file stem")?
        .to_string_lossy();
    let number = index
        .checked_add(1)
        .context("workbook part number overflow")?;
    Ok(path.with_file_name(format!("{stem}-{number:04}.xlsx")))
}

fn stage(
    path: &Path,
    records: &[&AthleteRecord],
    issues: &[ExportIssue],
    options: &ExportOptions,
    staged: &mut Vec<PathBuf>,
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
    workbook::write_and_verify(temporary, records, issues, options)?;
    file.sync_all().context("syncing verified workbook")
}

fn publish_parts(staged: &[PathBuf], destinations: &[PathBuf]) -> Result<()> {
    let mut published: Vec<&Path> = Vec::with_capacity(destinations.len());
    staged.iter().zip(destinations).try_for_each(|(temporary, path)| {
        fs::hard_link(temporary, path).with_context(|| {
            let published = published
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "publishing {} without overwrite failed; previously published parts remain: [{}]",
                path.display(),
                published
            )
        })?;
        published.push(path.as_path());
        Ok::<(), anyhow::Error>(())
    })
}

fn cleanup_staged(staged: &[PathBuf]) -> Result<()> {
    let errors = staged
        .iter()
        .filter_map(|path| {
            fs::remove_file(path)
                .err()
                .map(|error| format!("{}: {error}", path.display()))
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(errors.is_empty(), "removing temporary workbooks: {}", errors.join("; "));
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod publication_tests {
    use super::{cleanup_staged, publish_parts};
    use anyhow::Result;
    use std::fs;

    #[test]
    fn concurrent_destination_preserves_other_file_and_reports_published_part() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let staged = [directory.path().join("stage-1"), directory.path().join("stage-2")];
        let destinations = [directory.path().join("part-1"), directory.path().join("part-2")];
        fs::write(&staged[0], b"first workbook")?;
        fs::write(&staged[1], b"second workbook")?;
        fs::write(&destinations[1], b"other user's file")?;

        let error = publish_parts(&staged, &destinations)
            .err()
            .ok_or_else(|| anyhow::anyhow!("publication unexpectedly succeeded"))?;
        assert!(error.to_string().contains(&destinations[0].display().to_string()));
        assert_eq!(fs::read(&destinations[0])?, b"first workbook");
        assert_eq!(fs::read(&destinations[1])?, b"other user's file");
        cleanup_staged(&staged)?;
        assert_eq!(fs::read_dir(directory.path())?.count(), 2);
        Ok(())
    }
}
