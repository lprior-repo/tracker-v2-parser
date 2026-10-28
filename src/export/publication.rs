use anyhow::{Context, Result};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub(super) fn manifest_path(path: &Path) -> PathBuf {
    path.with_extension("manifest.json")
}

pub(super) fn publish(
    staged: &[PathBuf],
    destinations: &[PathBuf],
    manifest: Option<&Path>,
) -> Result<()> {
    anyhow::ensure!(
        staged.len() == destinations.len(),
        "staged workbook count mismatch"
    );
    let prepared = manifest
        .map(|path| prepare_manifest(path, staged, destinations))
        .transpose()?;
    let mut published = Vec::new();
    let result = staged
        .iter()
        .zip(destinations)
        .try_for_each(|(temporary, destination)| {
            fs::hard_link(temporary, destination).with_context(|| {
                format!("publishing {} without overwrite", destination.display())
            })?;
            published.push((temporary.as_path(), destination.as_path()));
            Ok::<(), anyhow::Error>(())
        })
        .and_then(|()| sync_destinations(destinations));
    let result = result.and_then(|()| {
        if let Some((temporary, destination)) = &prepared {
            fs::hard_link(temporary, destination)
                .context("publishing export completion manifest without overwrite")?;
            published.push((temporary.as_path(), destination.as_path()));
            sync_parent(destination)?;
        }
        Ok(())
    });
    let result = match result {
        Ok(()) => Ok(()),
        Err(error) => match rollback(&published) {
            Ok(()) => Err(error.context("publication incomplete; this invocation's links removed; no completion manifest published")),
            Err(cleanup) => Err(error.context(format!("publication incomplete; rollback failed: {cleanup:#}"))),
        },
    };
    let cleanup = prepared.map_or(Ok(()), |(temporary, _)| {
        fs::remove_file(&temporary).context("removing staged completion manifest")
    });
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => {
            Err(error.context("export completed, but staged manifest cleanup failed"))
        }
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("staged manifest cleanup failed: {cleanup:#}")))
        }
    }
}

fn prepare_manifest(
    path: &Path,
    staged: &[PathBuf],
    destinations: &[PathBuf],
) -> Result<(PathBuf, PathBuf)> {
    anyhow::ensure!(
        !path.try_exists()?,
        "completion manifest already exists: {}",
        path.display()
    );
    let parts = staged.iter().zip(destinations).map(|(temporary, destination)| {
        let mut input = File::open(temporary)?;
        let mut digest = DigestWriter(Sha256::new());
        let bytes = io::copy(&mut input, &mut digest)?;
        Ok(serde_json::json!({"file": destination.file_name().and_then(|name| name.to_str()).context("UTF-8 workbook file name")?, "bytes": bytes, "sha256": format!("{:x}", digest.0.finalize())}))
    }).collect::<Result<Vec<_>>>()?;
    let data = serde_json::to_vec_pretty(
        &serde_json::json!({"format": "track-scrapper-export-v1", "complete": true, "parts": parts}),
    )?;
    let sequence = super::TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .context("manifest file name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = file.write_all(&data).and_then(|()| file.sync_all());
    if let Err(error) = result {
        fs::remove_file(&temporary).context("removing failed staged completion manifest")?;
        return Err(error.into());
    }
    Ok((temporary, path.to_path_buf()))
}

fn rollback(published: &[(&Path, &Path)]) -> Result<()> {
    let errors = published
        .iter()
        .rev()
        .filter_map(|(temporary, destination)| rollback_entry(temporary, destination).err())
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
    Ok(())
}

struct RollbackClaim {
    expected: fs::Metadata,
    destination: PathBuf,
    directory: PathBuf,
    entry: PathBuf,
}

fn rollback_entry(temporary: &Path, destination: &Path) -> Result<()> {
    if let Some(claim) = claim_published(temporary, destination)? {
        finish_claim(&claim)?;
    }
    Ok(())
}

fn claim_published(temporary: &Path, destination: &Path) -> Result<Option<RollbackClaim>> {
    let expected = fs::metadata(temporary)?;
    let sequence = super::TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = destination
        .file_name()
        .context("published file name")?
        .to_string_lossy();
    let directory = destination.with_file_name(format!(
        ".{name}.rollback.{}.{}",
        std::process::id(),
        sequence
    ));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    if let Err(error) = sync_parent(&directory) {
        fs::remove_dir(&directory).with_context(|| {
            format!(
                "removing empty rollback directory {}; initial directory sync failed: {error:#}",
                directory.display()
            )
        })?;
        return Err(error);
    }
    let entry = directory.join("entry");
    let result = renameat_with(CWD, destination, CWD, &entry, RenameFlags::NOREPLACE);
    if let Err(error) = result {
        fs::remove_dir(&directory).with_context(|| {
            format!(
                "removing empty rollback directory {}; capture failed: {error}",
                directory.display()
            )
        })?;
        sync_parent(&directory)?;
        if error == rustix::io::Errno::NOENT {
            return Ok(None);
        }
        return Err(error)
            .with_context(|| format!("claiming published path {}", destination.display()));
    }
    let claim = RollbackClaim {
        expected,
        destination: destination.to_path_buf(),
        directory,
        entry,
    };
    sync_directory(&claim.directory)
        .and_then(|()| sync_directory(parent(&claim.destination)))
        .with_context(|| {
            format!(
                "syncing rollback claim; entry retained for recovery at {}",
                claim.entry.display()
            )
        })?;
    Ok(Some(claim))
}

fn finish_claim(claim: &RollbackClaim) -> Result<()> {
    let current = fs::symlink_metadata(&claim.entry).with_context(|| {
        format!(
            "inspecting rollback entry retained at {}",
            claim.entry.display()
        )
    })?;
    if claim.expected.dev() == current.dev() && claim.expected.ino() == current.ino() {
        fs::remove_file(&claim.entry)
            .with_context(|| format!("removing own rollback entry {}", claim.entry.display()))?;
        return finish_claim_directory(claim);
    }
    renameat_with(CWD, &claim.entry, CWD, &claim.destination, RenameFlags::NOREPLACE)
        .with_context(|| format!("published path changed; refusing to overwrite {}; unrelated entry retained for recovery at {}", claim.destination.display(), claim.entry.display()))?;
    sync_parent(&claim.destination)?;
    finish_claim_directory(claim)?;
    anyhow::bail!(
        "published path changed; unrelated entry restored without overwrite: {}",
        claim.destination.display()
    )
}

fn finish_claim_directory(claim: &RollbackClaim) -> Result<()> {
    sync_directory(&claim.directory)
        .with_context(|| format!("syncing rollback directory {}", claim.directory.display()))?;
    fs::remove_dir(&claim.directory).with_context(|| {
        format!(
            "removing empty rollback directory {}",
            claim.directory.display()
        )
    })?;
    sync_parent(&claim.directory)
}

fn sync_destinations(destinations: &[PathBuf]) -> Result<()> {
    if let Some(destination) = destinations.first() {
        sync_parent(destination)?;
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn sync_parent(path: &Path) -> Result<()> {
    sync_directory(parent(path)).context("syncing export directory")
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

struct DigestWriter(Sha256);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
