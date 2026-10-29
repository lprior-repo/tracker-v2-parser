use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};

use super::{
    WorkerInstallArgs, files,
    model::{self, Configuration, Receipt},
    plist,
};

pub(super) fn prepare(args: &WorkerInstallArgs, launcher: &Path, agents: &Path) -> Result<Receipt> {
    ensure!(args.port >= 1024, "worker port must be unprivileged");
    [
        args.runtime.as_path(),
        args.binary.as_path(),
        args.secrets.as_path(),
        launcher,
        agents,
    ]
    .into_iter()
    .try_for_each(|path| model::path_text(path).map(|_| ()))?;
    files::directory(&args.runtime, true)?;
    let runtime = fs::canonicalize(&args.runtime)?;
    files::read_private(&args.secrets, 65_536)?;
    let secrets = fs::canonicalize(&args.secrets)?;
    let digest = files::digest(&args.binary)?;
    let launcher_digest = files::digest(launcher)?;
    let deployment_digest = model::deployment_digest(&digest, &launcher_digest);
    let version = runtime.join("workers").join(&deployment_digest);
    files::directory(&runtime.join("workers"), true)?;
    files::directory(&version, true)?;
    let artifacts = runtime.join("artifacts");
    files::directory(&artifacts, true)?;
    let logs = version.join("logs");
    files::directory(&logs, true)?;
    let configuration = Configuration {
        binary: version.join("track-scrapper-v2"),
        binary_sha256: digest.clone(),
        launcher: version.join("native-ops"),
        launcher_sha256: launcher_digest.clone(),
        secrets,
        artifacts,
        bind: format!("127.0.0.1:{}", args.port),
        published_workbook: args
            .published_workbook
            .as_deref()
            .map(std::path::absolute)
            .transpose()?,
    };
    model::validate(&configuration)?;
    files::executable(&args.binary, &configuration.binary, &digest)?;
    files::executable(launcher, &configuration.launcher, &launcher_digest)?;
    let configuration_path = version.join("worker.json");
    let mut json = serde_json::to_vec_pretty(&configuration)?;
    json.push(b'\n');
    files::immutable_bytes(&configuration_path, &json)?;
    files::directory(agents, false)?;
    let agents = fs::canonicalize(agents)?;
    let label = format!(
        "local.scraperv2native.worker.{}",
        deployment_digest.chars().take(12).collect::<String>()
    );
    let plist_path = agents.join(format!("{label}.plist"));
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned());
    let body = plist::render(&configuration, &configuration_path, &logs, &label, &path)
        .context("rendering worker LaunchAgent")?;
    files::immutable_bytes(&plist_path, body.as_bytes())?;
    Ok(Receipt {
        configuration,
        label,
        plist: plist_path,
        activated: false,
    })
}
