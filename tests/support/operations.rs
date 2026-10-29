use std::{
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{ExitStatus, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use track_scrapper_v2::{
    config::Config,
    durable::operations::{self, OperationSpec, Workspace},
    model::Target,
    runner::Runner,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub enum OfflineOperation {
    Replay {
        config: Option<PathBuf>,
        input: PathBuf,
        target: Target,
    },
    Export {
        config: Option<PathBuf>,
        output: PathBuf,
        per_athlete: bool,
    },
    Directory {
        csv: PathBuf,
        manifest: PathBuf,
    },
    PublishedSchools {
        csv: PathBuf,
        sha256: String,
    },
    PrivateSchools {
        csv: PathBuf,
        sha256: String,
    },
    ReviewedIdentity {
        manifest: PathBuf,
        sha256: String,
    },
}

fn config(directory: &Path, path: Option<&Path>) -> Result<Config> {
    let default = directory.join("test-config.toml");
    if path.is_none() {
        super::write_config(&default)?;
    }
    Config::load(path.map_or(default.as_path(), std::convert::identity))
}

async fn imported(root: &Path, url: &str, operation: &OfflineOperation) -> Result<String> {
    let checked = |path: &Path, expected: &str| -> Result<_> {
        let artifact = operations::stage_input(root, path)?;
        ensure!(
            artifact.sha256.eq_ignore_ascii_case(expected),
            "input SHA-256 mismatch"
        );
        Ok(artifact)
    };
    let spec = match operation {
        OfflineOperation::Directory { csv, manifest } => OperationSpec::SchoolDirectory {
            csv: operations::stage_input(root, csv)?,
            manifest: operations::stage_input(root, manifest)?,
        },
        OfflineOperation::PublishedSchools { csv, sha256 } => OperationSpec::PublicSchools {
            csv: checked(csv, sha256)?,
            origin: None,
        },
        OfflineOperation::PrivateSchools { csv, sha256 } => OperationSpec::PrivateSchools {
            csv: checked(csv, sha256)?,
        },
        OfflineOperation::ReviewedIdentity { manifest, sha256 } => {
            OperationSpec::ReviewedIdentity {
                manifest: checked(manifest, sha256)?,
            }
        }
        _ => anyhow::bail!("offline import operation required"),
    };
    let id = format!("offline-test-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let workspace = Workspace::open(root, &id, &spec)?;
    Ok(serde_json::to_string(
        &operations::execute_import(&workspace, &spec, url).await?,
    )?)
}

async fn execute(
    directory: &Path,
    url: Option<&str>,
    operation: &OfflineOperation,
) -> Result<String> {
    let url = url.context("offline adapter requires an explicit database URL")?;
    match operation {
        OfflineOperation::Replay {
            config: path,
            input,
            target,
        } => {
            let config = config(directory, path.as_deref())?;
            Runner::new()
                .replay(
                    &config,
                    url,
                    input,
                    target.clone(),
                    &directory.join("evidence"),
                )
                .await?;
            Ok(String::new())
        }
        OfflineOperation::Export {
            config: path,
            output,
            per_athlete,
        } => {
            let mut config = config(directory, path.as_deref())?;
            config.export.per_athlete_sheets |= *per_athlete;
            Runner::new().export(&config, output, url).await?;
            Ok(String::new())
        }
        other => imported(&directory.join("operation-artifacts"), url, other).await,
    }
}

pub fn run<'a>(
    directory: &'a Path,
    url: Option<&'a str>,
    operation: &'a OfflineOperation,
) -> BoxFuture<'a, Result<Output>> {
    Box::pin(async move {
        let outcome = execute(directory, url, operation).await;
        Ok(match outcome {
            Ok(value) => Output {
                status: ExitStatus::from_raw(0),
                stdout: value.into_bytes(),
                stderr: Vec::new(),
            },
            Err(error) => Output {
                status: ExitStatus::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: format!("{error:#}").into_bytes(),
            },
        })
    })
}
