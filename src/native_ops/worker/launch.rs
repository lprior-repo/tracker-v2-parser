use std::{collections::BTreeMap, os::unix::process::CommandExt, path::Path, process::Command};

use anyhow::{Context, Result, ensure};

use super::{
    files,
    model::{self, Configuration},
};

pub(super) struct Prepared {
    pub configuration: Configuration,
    pub environment: BTreeMap<String, String>,
}

fn environment(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).context("private worker environment must be UTF-8")?;
    let values =
        text.lines()
            .enumerate()
            .try_fold(BTreeMap::new(), |mut values, (index, line)| {
                ensure!(index < 256, "private worker environment exceeds 256 lines");
                let (key, raw) = line
                    .split_once('=')
                    .context("private worker environment line must contain a JSON string")?;
                ensure!(
                    !key.is_empty()
                        && key.bytes().all(|byte| byte.is_ascii_uppercase()
                            || byte.is_ascii_digit()
                            || byte == b'_')
                        && !key.starts_with(|character: char| character.is_ascii_digit()),
                    "invalid private worker environment key"
                );
                let value: String = serde_json::from_str(raw).map_err(|_| {
                    anyhow::anyhow!("private worker environment value must be a JSON string")
                })?;
                ensure!(
                    value.len() <= 16_384 && !value.contains('\0'),
                    "invalid private worker environment value length or NUL"
                );
                ensure!(
                    values.insert(key.to_owned(), value).is_none(),
                    "duplicate private worker environment key"
                );
                Ok::<_, anyhow::Error>(values)
            })?;
    ensure!(
        values
            .get("DATABASE_URL")
            .is_some_and(|value| !value.is_empty()),
        "DATABASE_URL missing from private worker environment"
    );
    Ok(values)
}

pub(super) fn prepare(path: &Path) -> Result<Prepared> {
    model::path_text(path)?;
    let configuration: Configuration = serde_json::from_slice(&files::read_private(path, 65_536)?)
        .map_err(|_| anyhow::anyhow!("invalid private worker configuration JSON"))?;
    model::validate(&configuration)?;
    files::verify_executable(&configuration.binary, &configuration.binary_sha256)
        .context("verifying immutable worker binary")?;
    files::verify_executable(&configuration.launcher, &configuration.launcher_sha256)
        .context("verifying immutable launcher binary")?;
    files::directory(&configuration.artifacts, true)?;
    let environment = environment(&files::read_private(&configuration.secrets, 65_536)?)?;
    Ok(Prepared {
        configuration,
        environment,
    })
}

pub(super) fn command(prepared: &Prepared) -> Command {
    let configuration = &prepared.configuration;
    let mut command = Command::new(&configuration.binary);
    command
        .args([
            "durable",
            "serve",
            "--bind",
            &configuration.bind,
            "--artifacts",
        ])
        .arg(&configuration.artifacts)
        .envs(&prepared.environment);
    if let Some(path) = &configuration.published_workbook {
        command.arg("--published-workbook").arg(path);
    }
    command
}

pub(super) fn execute(prepared: &Prepared) -> Result<()> {
    Err(command(prepared).exec()).context("executing verified native worker")
}
