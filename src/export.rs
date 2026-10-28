mod capacity;
mod profiles;
mod publication;
mod rows;
mod selection;
mod single_sheet;
mod workbook;

use crate::model::AthleteRecord;
use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportLayout {
    #[default]
    Detailed,
    SingleSheet,
}

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
    validate(path, options)?;
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
        stage(
            path,
            &[],
            issues,
            options,
            &mut staged,
            ExportLayout::Detailed,
        )
    } else {
        selected
            .chunks(options.rows_per_file)
            .zip(&destinations)
            .enumerate()
            .try_for_each(|(index, (chunk, destination))| {
                let part_issues = if index == 0 { issues } else { &[] };
                stage(
                    destination,
                    chunk,
                    part_issues,
                    options,
                    &mut staged,
                    ExportLayout::Detailed,
                )
            })
    };
    let manifest = publication::manifest_path(path);
    let result =
        staging.and_then(|()| publication::publish(&staged, &destinations, Some(&manifest)));
    let cleanup = cleanup_staged(&staged);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(destinations),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => {
            Err(error
                .context("all workbook parts were published, but temporary-file cleanup failed"))
        }
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
    layout: ExportLayout,
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
    match layout {
        ExportLayout::Detailed => workbook::write_and_verify(temporary, records, issues, options)?,
        ExportLayout::SingleSheet => single_sheet::write_and_verify(temporary, records)?,
    }
    file.sync_all().context("syncing verified workbook")
}

fn validate(path: &Path, options: &ExportOptions) -> Result<()> {
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
    let manifest = publication::manifest_path(path);
    anyhow::ensure!(
        !manifest.try_exists()?,
        "completion manifest already exists: {}",
        manifest.display()
    );
    Ok(())
}

pub struct StagedExport {
    path: PathBuf,
    options: ExportOptions,
    staged: Vec<PathBuf>,
    destinations: Vec<PathBuf>,
    layout: ExportLayout,
}

impl StagedExport {
    pub fn new(path: &Path, options: &ExportOptions) -> Result<Self> {
        validate(path, options)?;
        Ok(Self {
            path: path.to_path_buf(),
            options: options.clone(),
            staged: Vec::new(),
            destinations: Vec::new(),
            layout: ExportLayout::Detailed,
        })
    }

    #[must_use]
    pub const fn with_layout(mut self, layout: ExportLayout) -> Self {
        self.layout = layout;
        self
    }

    pub fn stage_part(&mut self, records: &[AthleteRecord], issues: &[ExportIssue]) -> Result<()> {
        if self.layout == ExportLayout::SingleSheet && records.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            self.staged.len() == self.destinations.len(),
            "cannot continue a failed staged export"
        );
        anyhow::ensure!(
            self.destinations.len() < 10_000,
            "export exceeds 10000 workbook parts; select a narrower scope"
        );
        anyhow::ensure!(
            records.len() <= self.options.rows_per_file.min(1_000),
            "staged export batch exceeds configured/1000 athlete limit"
        );
        anyhow::ensure!(
            issues.len() <= 1_000,
            "staged export batch exceeds 1000 issue limit"
        );
        let path = destination(&self.path, self.destinations.len(), true)?;
        let records = records
            .iter()
            .filter(|record| selection::in_scope(record, &self.options))
            .collect::<Vec<_>>();
        stage(
            &path,
            &records,
            issues,
            &self.options,
            &mut self.staged,
            self.layout,
        )?;
        self.destinations.push(path);
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<PathBuf>> {
        if self.destinations.is_empty() {
            let path = destination(&self.path, 0, true)?;
            stage(
                &path,
                &[],
                &[],
                &self.options,
                &mut self.staged,
                self.layout,
            )?;
            self.destinations.push(path);
        }
        if self.destinations.len() == 1 {
            let destination = self
                .destinations
                .first_mut()
                .context("missing staged destination")?;
            destination.clone_from(&self.path);
        }
        let manifest = publication::manifest_path(&self.path);
        publication::publish(&self.staged, &self.destinations, Some(&manifest))?;
        cleanup_staged(&self.staged)?;
        self.staged.clear();
        Ok(std::mem::take(&mut self.destinations))
    }
}

impl Drop for StagedExport {
    fn drop(&mut self) {
        if let Err(error) = cleanup_staged(&self.staged) {
            tracing::error!(error = %error, "cleaning abandoned staged export");
        }
    }
}

fn cleanup_staged(staged: &[PathBuf]) -> Result<()> {
    let errors = staged
        .iter()
        .filter_map(|path| {
            fs::remove_file(path)
                .err()
                .filter(|error| error.kind() != std::io::ErrorKind::NotFound)
                .map(|error| format!("{}: {error}", path.display()))
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        errors.is_empty(),
        "removing temporary workbooks: {}",
        errors.join("; ")
    );
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod publication_tests {
    use super::{cleanup_staged, publication};
    use anyhow::Result;
    use std::fs;

    #[test]
    fn concurrent_destination_preserves_other_file_and_removes_own_published_links() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let staged = [
            directory.path().join("stage-1"),
            directory.path().join("stage-2"),
        ];
        let destinations = [
            directory.path().join("part-1"),
            directory.path().join("part-2"),
        ];
        fs::write(&staged[0], b"first workbook")?;
        fs::write(&staged[1], b"second workbook")?;
        fs::write(&destinations[1], b"other user's file")?;

        let error = publication::publish(&staged, &destinations, None)
            .err()
            .ok_or_else(|| anyhow::anyhow!("publication unexpectedly succeeded"))?;
        assert!(error.to_string().contains("publication incomplete"));
        assert!(!destinations[0].exists());
        assert_eq!(fs::read(&destinations[1])?, b"other user's file");
        cleanup_staged(&staged)?;
        assert_eq!(fs::read_dir(directory.path())?.count(), 1);
        Ok(())
    }
}
