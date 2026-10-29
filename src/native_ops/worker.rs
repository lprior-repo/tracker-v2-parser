mod files;
mod install;
mod launch;
mod model;
mod plist;

#[cfg(test)]
mod tests;

use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result, ensure};
use clap::Args;

#[derive(Args, Debug)]
pub struct WorkerInstallArgs {
    #[arg(long)]
    binary: PathBuf,
    #[arg(long)]
    runtime: PathBuf,
    #[arg(long)]
    secrets: PathBuf,
    #[arg(long, default_value_t = 9085, value_parser = clap::value_parser!(u16).range(1024..))]
    port: u16,
    #[arg(long)]
    activate: bool,
    #[arg(long)]
    published_workbook: Option<PathBuf>,
    #[arg(long)]
    launch_agents: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct WorkerLaunchArgs {
    #[arg(long)]
    config: PathBuf,
}

#[tracing::instrument(skip_all)]
pub async fn run_install(args: WorkerInstallArgs) -> Result<()> {
    let activate = args.activate;
    let mut receipt = tokio::task::spawn_blocking(move || {
        let launcher = std::env::current_exe().context("locating native-ops launcher")?;
        let agents = match &args.launch_agents {
            Some(path) => path.clone(),
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is required")?)
                .join("Library/LaunchAgents"),
        };
        install::prepare(&args, &launcher, &agents)
    })
    .await
    .context("joining worker preparation")??;
    if activate {
        activate_agent(&receipt.plist).await?;
        receipt.activated = true;
    }
    let mut output = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, &receipt)?;
    writeln!(output)?;
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn activate_agent(plist: &std::path::Path) -> Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("/usr/bin/id")
            .arg("-u")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("finding LaunchAgent user exceeded 30 seconds")?
    .context("finding LaunchAgent user")?;
    ensure!(output.status.success(), "finding LaunchAgent user failed");
    let uid = std::str::from_utf8(&output.stdout)?.trim().parse::<u32>()?;
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("/bin/launchctl")
            .args(["bootstrap", &format!("gui/{uid}")])
            .arg(plist)
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("worker LaunchAgent bootstrap exceeded 30 seconds")?
    .context("bootstrapping worker LaunchAgent")?;
    ensure!(status.success(), "worker LaunchAgent bootstrap failed");
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn run_launch(args: WorkerLaunchArgs) -> Result<()> {
    let prepared = tokio::task::spawn_blocking(move || launch::prepare(&args.config))
        .await
        .context("joining worker launch verification")??;
    launch::execute(&prepared)
}
