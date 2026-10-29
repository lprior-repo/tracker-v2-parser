mod capacity;
mod prepared;
mod prepared_verify;
mod profiles;
mod publication;
pub(crate) mod review;
mod rows;
mod selection;
mod single_sheet;
mod staging;
mod types;
mod workbook;

pub use prepared::{
    PreparedChunk, prepare_chunk, verify_chunk_source, verify_chunks, verify_chunks_with_review,
};
pub use review::{ReviewRow, ReviewStatus};
pub use types::{ExportIssue, ExportLayout, ExportOptions};

use crate::model::AthleteRecord;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        staging::detailed(path, &[], issues, options, &mut staged)
    } else {
        selected
            .chunks(options.rows_per_file)
            .zip(&destinations)
            .enumerate()
            .try_for_each(|(index, (chunk, destination))| {
                let part_issues = if index == 0 { issues } else { &[] };
                staging::detailed(destination, chunk, part_issues, options, &mut staged)
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
    started_layout: Option<ExportLayout>,
    single_sheet: single_sheet::Accumulator,
    review: Option<Vec<ReviewRow>>,
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
            started_layout: None,
            single_sheet: single_sheet::Accumulator::new(path),
            review: None,
        })
    }

    #[must_use]
    pub const fn with_layout(mut self, layout: ExportLayout) -> Self {
        self.layout = layout;
        self
    }

    pub fn stage_part(&mut self, records: &[AthleteRecord], issues: &[ExportIssue]) -> Result<()> {
        anyhow::ensure!(
            self.started_layout
                .is_none_or(|layout| layout == self.layout),
            "cannot change export layout after staging"
        );
        self.started_layout = Some(self.layout);
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
            .filter(|record| {
                self.layout != ExportLayout::SingleSheet
                    || record
                        .graduation_year
                        .is_some_and(|year| self.options.graduation_years.contains(&year))
            })
            .collect::<Vec<_>>();
        if self.layout == ExportLayout::SingleSheet {
            return self
                .single_sheet
                .append(&records, self.options.rows_per_file);
        }
        staging::detailed(&path, &records, issues, &self.options, &mut self.staged)?;
        self.destinations.push(path);
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<PathBuf>> {
        anyhow::ensure!(
            self.started_layout
                .is_none_or(|layout| layout == self.layout),
            "cannot change export layout after staging"
        );
        if self.layout == ExportLayout::SingleSheet {
            staging::with_writer(&self.path, &mut self.staged, |path| {
                self.single_sheet
                    .write_and_verify(path, self.review.as_deref())
            })?;
            self.destinations.push(self.path.clone());
        } else if self.destinations.is_empty() {
            let path = destination(&self.path, 0, true)?;
            staging::detailed(&path, &[], &[], &self.options, &mut self.staged)?;
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
mod publication_tests;
