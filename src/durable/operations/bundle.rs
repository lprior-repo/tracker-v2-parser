use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{Artifact, stage_input};

fn capture(root: &Path, item: &mut Value) -> Result<()> {
    let url = url::Url::parse(
        item.get("url")
            .and_then(Value::as_str)
            .context("WIAA capture URL")?,
    )?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("schools.wiaawi.org")
            && url.username().is_empty()
            && url.password().is_none(),
        "co-op capture requires public WIAA authority"
    );
    let expected = item
        .get("sha256")
        .and_then(Value::as_str)
        .context("WIAA capture SHA256")?
        .to_owned();
    let bytes = item
        .get("bytes")
        .and_then(Value::as_u64)
        .context("WIAA capture bytes")?;
    ensure!(
        bytes <= 8 * 1024 * 1024 && expected.len() == 64,
        "co-op capture exceeds metadata bounds"
    );
    let path = item
        .get("artifact_path")
        .and_then(Value::as_str)
        .context("WIAA capture path")?;
    let artifact = stage_input(root, Path::new(path))?;
    ensure!(
        artifact.sha256 == expected && artifact.bytes == bytes,
        "co-op capture staging hash/length mismatch"
    );
    item.as_object_mut()
        .context("WIAA capture metadata must be an object")?
        .insert(
            "artifact_path".into(),
            serde_json::to_value(&artifact.path)?,
        );
    Ok(())
}

pub fn stage_cooperative_manifest(root: &Path, source: &Path) -> Result<Artifact> {
    let mut bytes = Vec::new();
    File::open(source)?.take(262_145).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 262_144, "co-op manifest exceeds 256 KiB");
    let original = stage_input(root, source)?;
    ensure!(
        original.sha256 == super::artifacts::digest(&bytes)
            && original.bytes == u64::try_from(bytes.len())?,
        "co-op manifest changed during staging"
    );
    let mut manifest: Value = serde_json::from_slice(&bytes)?;
    let reports = manifest
        .get_mut("program_reports")
        .and_then(Value::as_array_mut)
        .context("co-op report metadata")?;
    ensure!(reports.len() == 4, "co-op import requires four reports");
    reports
        .iter_mut()
        .try_for_each(|item| capture(root, item))?;
    let associations = manifest
        .get_mut("associations")
        .and_then(Value::as_array_mut)
        .context("co-op associations")?;
    ensure!(associations.len() <= 64, "co-op association limit exceeded");
    associations.iter_mut().try_for_each(|item| -> Result<()> {
        capture(
            root,
            item.get_mut("host_capture").context("co-op host capture")?,
        )?;
        if let Some(schedules) = item
            .get_mut("current_schedules")
            .and_then(Value::as_array_mut)
        {
            ensure!(schedules.len() <= 4, "co-op schedule capacity exceeded");
            schedules
                .iter_mut()
                .try_for_each(|item| capture(root, item))?;
        }
        Ok(())
    })?;
    manifest
        .as_object_mut()
        .context("co-op manifest must be an object")?
        .insert(
            "_durable_original_manifest".into(),
            serde_json::to_value(original)?,
        );
    super::artifacts::store_input(root, &serde_json::to_vec(&manifest)?)
}
