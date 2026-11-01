use std::{
    fs::File,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{model::PrepareArgs, telemetry};

pub(super) fn stage(args: &PrepareArgs, prepared: &Value) -> Result<()> {
    let original = read_prior(&args.prior_config, &args.expected_prior_sha256)?;
    let root = args
        .prior_config
        .parent()
        .context("prior configuration has no parent")?
        .canonicalize()?;
    [&args.output, &args.candidate_config]
        .iter()
        .try_for_each(|target| {
            ensure!(
                target.is_absolute(),
                "configuration staging paths must be absolute"
            );
            let parent = target
                .parent()
                .context("configuration staging path needs parent")?
                .canonicalize()?;
            ensure!(
                !parent.starts_with(&root),
                "configuration staging must be outside live PostgreSQL data directory"
            );
            Ok::<_, anyhow::Error>(())
        })?;
    ensure!(
        !args.output.try_exists()?,
        "configuration receipt must be a new file"
    );
    ensure!(
        args.output != args.candidate_config,
        "candidate and receipt paths must differ"
    );
    let overrides = prepared
        .get("configuration")
        .and_then(Value::as_str)
        .context("prepared configuration missing")?;
    let candidate = format!("{original}\n{overrides}");
    let bytes = candidate.as_bytes();
    let hash = format!("{:x}", Sha256::digest(bytes));
    if args.candidate_config.try_exists()? {
        ensure!(
            read_prior(&args.candidate_config, &hash)?.as_bytes() == bytes,
            "existing candidate differs from exact prepared configuration"
        );
    } else {
        telemetry::save_bytes(&args.candidate_config, bytes)?;
    }
    telemetry::save(
        &args.output,
        &json!({"format":"native-postgres-staging-v1","applied":false,
        "prior_configuration":{"path":args.prior_config,"sha256":args.expected_prior_sha256},
        "candidate_configuration":{"path":args.candidate_config,"sha256":hash,"bytes":bytes.len()},
        "prepared":prepared,
        "installation":"Parent operator must verify prior SHA again and qualified backup/handoff evidence before replacement; this command does not install or reload PostgreSQL"}),
    )
}

fn read_prior(path: &Path, expected: &str) -> Result<String> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "configuration SHA256 must contain64hexadecimal characters"
    );
    ensure!(path.is_absolute(), "prior configuration must be absolute");
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() <= 1024 * 1024
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.permissions().mode().trailing_zeros() >= 6,
        "prior configuration must be a bounded owner-private regular file"
    );
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len())?);
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? == metadata.len(),
        "configuration changed while reading"
    );
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == expected.to_ascii_lowercase(),
        "configuration differs from expected prior SHA256; live configuration retained"
    );
    Ok(String::from_utf8(bytes)?)
}

#[cfg(test)]
mod tests;
