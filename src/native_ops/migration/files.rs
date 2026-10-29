use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::{
    manifest::{Fingerprint, MAX_FILES},
    paths,
};

pub(super) fn directory(path: &Path) -> Result<()> {
    paths::absolute(path)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}

pub(super) fn create(path: &Path) -> Result<File> {
    paths::absolute(path)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("creating new private migration file")
}

pub(super) fn open(path: &Path) -> Result<File> {
    paths::source(path, false)?;
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let file = File::from(descriptor);
    ensure!(
        file.metadata()?.file_type().is_file(),
        "source is no longer a regular file"
    );
    Ok(file)
}

fn signature(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

pub(super) fn copy(source: &Path, destination: &Path) -> Result<()> {
    let before = fs::symlink_metadata(source)?;
    let mut input = open(source)?;
    let opened = input.metadata()?;
    ensure!(
        signature(&before) == signature(&opened),
        "source changed while opening"
    );
    directory(destination.parent().context("payload has no parent")?)?;
    let mut output = create(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    ensure!(
        signature(&opened) == signature(&input.metadata()?)
            && signature(&before) == signature(&fs::symlink_metadata(source)?),
        "source changed during backup"
    );
    Ok(())
}

pub(super) fn hash_reader(input: &mut impl Read) -> Result<(u64, String)> {
    let mut writer = HashWriter(Sha256::new());
    let bytes = std::io::copy(input, &mut writer)?;
    Ok((bytes, format!("{:x}", writer.0.finalize())))
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn fingerprint(root: &Path, path: &Path) -> Result<Fingerprint> {
    let name = path
        .strip_prefix(root)?
        .to_str()
        .context("payload path is not UTF-8")?
        .to_owned();
    let (bytes, sha256) = hash_reader(&mut open(path)?)?;
    Ok(Fingerprint {
        path: name,
        bytes,
        sha256,
    })
}

pub(super) fn list(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut queue = vec![root.to_owned()];
    let mut files = Vec::new();
    (0..MAX_FILES).try_for_each(|_| {
        let Some(directory) = queue.pop() else {
            return Ok(());
        };
        fs::read_dir(directory)?.try_for_each(|entry| {
            let entry = entry?;
            let path = paths::source(&entry.path(), entry.file_type()?.is_dir())?;
            ensure!(
                files
                    .len()
                    .checked_add(queue.len())
                    .context("file count overflow")?
                    < MAX_FILES,
                "too many payload files"
            );
            if entry.file_type()?.is_dir() {
                queue.push(path);
            } else {
                files.push(path);
            }
            Ok::<_, anyhow::Error>(())
        })
    })?;
    ensure!(queue.is_empty(), "payload directory count exceeds limit");
    files.sort();
    Ok(files)
}

pub(super) fn json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut file = create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn load<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let file = open(path)?;
    ensure!(
        file.metadata()?.len() <= limit,
        "JSON metadata exceeds limit"
    );
    Ok(serde_json::from_reader(file.take(
        limit.checked_add(1).context("metadata bound overflow")?,
    ))?)
}
