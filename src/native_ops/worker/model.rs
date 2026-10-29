use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Configuration {
    pub binary: PathBuf,
    pub binary_sha256: String,
    pub launcher: PathBuf,
    pub launcher_sha256: String,
    pub secrets: PathBuf,
    pub artifacts: PathBuf,
    pub bind: String,
    pub published_workbook: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(super) struct Receipt {
    #[serde(flatten)]
    pub configuration: Configuration,
    pub label: String,
    pub plist: PathBuf,
    pub activated: bool,
}

pub(super) fn deployment_digest(worker: &str, launcher: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"native-worker-v1:");
    digest.update(worker.as_bytes());
    digest.update(launcher.as_bytes());
    format!("{:x}", digest.finalize())
}

pub(super) fn path_text(path: &Path) -> Result<&str> {
    let value = path.to_str().context("worker paths must be UTF-8")?;
    ensure!(value.len() <= 4096, "worker path exceeds 4096 bytes");
    valid_text(value)?;
    Ok(value)
}

pub(super) fn valid_text(value: &str) -> Result<()> {
    ensure!(!value.chars().any(|character| character.is_control() || matches!(character, '\u{fffe}' | '\u{ffff}')), "worker configuration contains invalid control characters");
    Ok(())
}

pub(super) fn validate(configuration: &Configuration) -> Result<()> {
    [
        &configuration.binary,
        &configuration.launcher,
        &configuration.secrets,
        &configuration.artifacts,
    ]
    .into_iter()
    .try_for_each(|path| {
        ensure!(path.is_absolute(), "worker paths must be absolute");
        path_text(path).map(|_| ())
    })?;
    if let Some(path) = &configuration.published_workbook {
        ensure!(
            path.is_absolute(),
            "published workbook path must be absolute"
        );
        path_text(path)?;
    }
    [&configuration.binary_sha256, &configuration.launcher_sha256]
        .into_iter()
        .try_for_each(|digest| {
            ensure!(
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "worker SHA-256 must contain 64 hexadecimal characters"
            );
            Ok::<_, anyhow::Error>(())
        })?;
    let bind: SocketAddr = configuration.bind.parse().context("invalid worker bind")?;
    ensure!(
        bind.ip().is_loopback() && bind.port() >= 1024,
        "worker must bind an unprivileged loopback port"
    );
    Ok(())
}
