use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::{ArtifactRef, CaptureRef};
use crate::model::Capture;

pub(in crate::durable) const MAX_PARSED_BYTES: usize = 64 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn address(root: &Path, reference: &ArtifactRef, limit: usize) -> Result<PathBuf> {
    super::binding::validate_digest(&reference.sha256)?;
    ensure!(root.is_absolute(), "durable artifact root must be absolute");
    ensure!(
        reference.bytes <= u64::try_from(limit)?,
        "durable artifact exceeds its byte grant"
    );
    Ok(root.join(format!("{}.blob", reference.sha256)))
}

fn read_sync(root: &Path, reference: &ArtifactRef, limit: usize) -> Result<Vec<u8>> {
    let path = address(root, reference, limit)?;
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_file(),
        "durable artifact must be a regular file"
    );
    let file = File::open(path).context("opening durable artifact")?;
    ensure!(
        file.metadata()?.len() == reference.bytes,
        "durable artifact size mismatch"
    );
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(reference.bytes)?)?;
    file.take(
        reference
            .bytes
            .checked_add(1)
            .context("artifact size overflow")?,
    )
    .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? == reference.bytes,
        "durable artifact changed while reading"
    );
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == reference.sha256,
        "durable artifact digest mismatch"
    );
    Ok(bytes)
}

fn write_sync(root: &Path, bytes: &[u8], limit: usize) -> Result<ArtifactRef> {
    ensure!(
        bytes.len() <= limit,
        "durable artifact exceeds its byte grant"
    );
    let reference = ArtifactRef {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        bytes: u64::try_from(bytes.len())?,
    };
    let path = address(root, &reference, limit)?;
    fs::create_dir_all(root)?;
    File::open(
        root.parent()
            .context("durable artifact root has no parent")?,
    )?
    .sync_all()?;
    let temporary = root.join(format!(
        ".{}.{}.{}.tmp",
        reference.sha256,
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = publish(&mut file, root, &temporary, &path, bytes, &reference, limit);
    let cleanup = fs::remove_file(&temporary);
    result?;
    cleanup.context("removing staged durable artifact")?;
    Ok(reference)
}

fn publish(
    file: &mut File,
    root: &Path,
    temporary: &Path,
    path: &Path,
    bytes: &[u8],
    reference: &ArtifactRef,
    limit: usize,
) -> Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    match fs::hard_link(temporary, path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                read_sync(root, reference, limit)? == bytes,
                "durable artifact content mismatch"
            );
        }
        Err(error) => return Err(error).context("publishing durable artifact"),
    }
    File::open(root)?
        .sync_all()
        .context("syncing durable artifact directory")
}

pub(super) async fn write(root: &Path, bytes: Vec<u8>, limit: usize) -> Result<ArtifactRef> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || write_sync(&root, &bytes, limit))
        .await
        .context("durable artifact writer failed")?
}

pub(in crate::durable) async fn read(
    root: &Path,
    reference: &ArtifactRef,
    limit: usize,
) -> Result<Vec<u8>> {
    let root = root.to_owned();
    let reference = reference.clone();
    tokio::task::spawn_blocking(move || read_sync(&root, &reference, limit))
        .await
        .context("durable artifact reader failed")?
}

pub(super) async fn capture(root: &Path, reference: &CaptureRef) -> Result<Capture> {
    Ok(Capture {
        url: reference.url.clone(),
        status: reference.status,
        content_type: reference.content_type.clone(),
        body: read(root, &reference.body, crate::transport::MAX_BODY_BYTES).await?,
    })
}

pub(super) fn attach(
    root: &Path,
    reference: &CaptureRef,
    requested_url: &str,
    parsed: &mut crate::model::ParsedPage,
) -> Result<()> {
    let path = address(root, &reference.body, crate::transport::MAX_BODY_BYTES)?;
    let evidence = serde_json::json!({ "artifact_path": path, "sha256": reference.body.sha256, "bytes": reference.body.bytes, "source_url": reference.url, "http_status": reference.status, "content_type": reference.content_type, "retained_at": chrono::DateTime::from_timestamp_millis(reference.retained_at_ms).context("invalid capture timestamp")?, "representation": "transport_sanitized_response" });
    parsed
        .athletes
        .iter_mut()
        .try_for_each(|athlete| -> Result<()> {
            let values = athlete
                .source_values
                .get_or_insert_with(|| serde_json::json!({}));
            let object = values
                .as_object_mut()
                .context("athlete source values must be an object")?;
            if let Some(page) = object
                .get_mut("pages")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|pages| pages.get_mut(requested_url))
                .and_then(serde_json::Value::as_object_mut)
            {
                page.insert("_capture".to_owned(), evidence.clone());
            }
            object.insert("_capture".to_owned(), evidence.clone());
            Ok(())
        })
}
