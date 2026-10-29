use std::{path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

#[tracing::instrument(skip_all)]
async fn brew(args: &[&str]) -> Result<String> {
    let result = tokio::time::timeout(
        Duration::from_mins(30),
        Command::new("brew")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Homebrew deadline exceeded")?
    .context("Homebrew must already be installed")?;
    ensure!(
        result.status.success(),
        "Homebrew failed; inspect locally (diagnostics withheld)"
    );
    ensure!(
        result.stdout.len() <= 1024 * 1024,
        "Homebrew output exceeds bound"
    );
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

#[tracing::instrument(skip_all)]
pub(super) async fn run(major: u32) -> Result<PathBuf> {
    ensure!(
        cfg!(target_os = "macos"),
        "--bootstrap-macos requires macOS"
    );
    let host = super::database::host()?;
    ensure!(
        ["localhost", "127.0.0.1", "::1"].contains(&host.as_str()) || host.starts_with('/'),
        "Homebrew bootstrap requires local PostgreSQL"
    );
    let formula = format!("postgresql@{major}");
    let installed = brew(&["list", "--formula", "--versions"]).await?;
    if !installed
        .lines()
        .any(|line| line.split_whitespace().next() == Some(formula.as_str()))
    {
        brew(&["install", &formula]).await?;
    }
    let prefix = brew(&["--prefix", &formula]).await?;
    ensure!(
        !prefix.contains('\n') && PathBuf::from(&prefix).is_absolute(),
        "Homebrew returned invalid prefix"
    );
    brew(&["services", "start", &formula]).await?;
    let directory = PathBuf::from(prefix).join("bin");
    let ready = futures::stream::iter(0..30)
        .then(|_| async {
            let status = tokio::time::timeout(
                Duration::from_secs(4),
                Command::new(directory.join("pg_isready"))
                    .args(["--dbname=postgres", "--timeout=2"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await;
            if matches!(status,Ok(Ok(status)) if status.success()) {
                return true;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            false
        })
        .any(|ready| async move { ready })
        .await;
    ensure!(
        ready,
        "local PostgreSQL did not become ready; installed Homebrew resources are retained"
    );
    Ok(directory)
}

use futures::StreamExt;
