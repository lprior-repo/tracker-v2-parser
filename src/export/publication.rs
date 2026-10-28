use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
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
    published
        .iter()
        .rev()
        .try_for_each(|(temporary, destination)| {
            let own = fs::metadata(temporary)?;
            let current = match fs::symlink_metadata(destination) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            anyhow::ensure!(
                own.dev() == current.dev() && own.ino() == current.ino(),
                "published path changed; refusing to remove {}",
                destination.display()
            );
            fs::remove_file(destination)?;
            sync_parent(destination)
        })
}

fn sync_destinations(destinations: &[PathBuf]) -> Result<()> {
    if let Some(destination) = destinations.first() {
        sync_parent(destination)?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    File::open(parent)?
        .sync_all()
        .context("syncing export directory")
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
