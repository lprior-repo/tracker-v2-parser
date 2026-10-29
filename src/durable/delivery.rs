use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::operations::{Artifact, PublishedExport};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn publish(path: &Path, export: &PublishedExport) -> Result<bool> {
    ensure!(
        path.is_absolute(),
        "published workbook path must be absolute"
    );
    ensure!(
        path.extension()
            .is_some_and(|extension| extension == "xlsx"),
        "published workbook must have an xlsx extension"
    );
    let root = export
        .canonical_pointer
        .parent()
        .context("canonical export parent")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("canonical-publication.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    let mut bytes = Vec::new();
    File::open(&export.canonical_pointer)?
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "canonical pointer exceeds 1 MiB");
    let current: PublishedExport = serde_json::from_slice(&bytes)?;
    if !export.promoted || current.binding != export.binding || current.workbook != export.workbook
    {
        return Ok(false);
    }
    let directory = path.parent().context("published workbook parent")?;
    ensure!(directory.is_dir(), "published workbook parent must exist");
    replace_file(path, &export.workbook)?;
    replace_file(&path.with_extension("manifest.json"), &export.manifest)?;
    File::open(directory)?.sync_all()?;
    Ok(true)
}

fn replace_file(destination: &Path, source: &Artifact) -> Result<()> {
    ensure!(
        source.bytes <= 67_108_864,
        "workbook delivery exceeds 64 MiB"
    );
    let mut bytes = Vec::new();
    File::open(&source.path)?
        .take(67_108_865)
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? == source.bytes
            && format!("{:x}", Sha256::digest(&bytes)) == source.sha256,
        "verified delivery artifact hash/length mismatch"
    );
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_file_name(format!(
        ".durable-delivery-{}-{sequence}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .context("staging verified workbook delivery")?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if let Err(error) = fs::rename(&temporary, destination) {
        let cleanup = fs::remove_file(&temporary);
        return match cleanup {
            Ok(()) => Err(error).context("publishing verified workbook delivery"),
            Err(cleanup) => Err(error).context(format!("delivery cleanup also failed: {cleanup}")),
        };
    }
    Ok(())
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
