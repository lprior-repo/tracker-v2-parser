use super::ArtifactRef;
#[cfg(target_os = "macos")]
use anyhow::{Result, ensure};
#[cfg(target_os = "macos")]
use std::fs::File;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "macos")]
mod process;

pub(super) struct Stage {
    directory: PathBuf,
    path: PathBuf,
}

impl Stage {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        report_cleanup(fs::remove_file(&self.path));
        report_cleanup(fs::remove_dir(&self.directory));
    }
}

fn report_cleanup(result: std::io::Result<()>) {
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error,"retaining unreconciled artifact compression stage");
    }
}

#[cfg(target_os = "macos")]
pub(super) fn prepare(
    root: &Path,
    raw: &Path,
    reference: &ArtifactRef,
    bytes: &[u8],
) -> Option<Stage> {
    match prepare_with(root, raw, reference, bytes, copy) {
        Ok(stage) => stage,
        Err(error) => {
            tracing::debug!(%error,"artifact compression unavailable; retaining raw publication");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) const fn prepare(
    _root: &Path,
    _raw: &Path,
    _reference: &ArtifactRef,
    _bytes: &[u8],
) -> Option<Stage> {
    None
}

#[cfg(target_os = "macos")]
fn prepare_with(
    root: &Path,
    raw: &Path,
    reference: &ArtifactRef,
    bytes: &[u8],
    copy_body: fn(&Path, &Path) -> Result<()>,
) -> Result<Option<Stage>> {
    use std::os::macos::fs::MetadataExt;
    if bytes.len() < 65536 {
        return Ok(None);
    }
    let name = raw
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("unpublished artifact name absent"))?;
    let directory = root.join(name).with_extension("compression-stage");
    fs::create_dir(&directory)?;
    let stage = Stage {
        path: directory.join(format!("{}.blob", reference.sha256)),
        directory,
    };
    copy_body(raw, &stage.path)?;
    ensure!(
        super::read_sync(&stage.directory, reference, bytes.len())? == bytes,
        "compressed artifact bytes differ"
    );
    let original = fs::symlink_metadata(raw)?;
    let compressed = fs::symlink_metadata(&stage.path)?;
    ensure!(
        compressed.permissions() == original.permissions()
            && compressed.st_uid() == original.st_uid()
            && compressed.st_gid() == original.st_gid(),
        "compressed artifact permissions differ"
    );
    if compressed.st_flags() & 0x20 == 0 || compressed.st_blocks() >= original.st_blocks() {
        return Ok(None);
    }
    File::open(&stage.path)?.sync_all()?;
    Ok(Some(stage))
}

#[cfg(target_os = "macos")]
fn copy(raw: &Path, path: &Path) -> Result<()> {
    let mut command = std::process::Command::new("/usr/bin/ditto");
    command
        .args([
            "--hfsCompression",
            "--noclone",
            "--rsrc",
            "--extattr",
            "--acl",
        ])
        .arg(raw)
        .arg(path);
    process::run(command, 200)
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "macos"))]
mod benchmark;
