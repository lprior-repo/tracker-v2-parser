use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

const MAX_BINARY: u64 = 536_870_912;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn open_regular(path: &Path, private: bool, limit: u64) -> Result<File> {
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .context("opening regular worker file")?;
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "worker file is not regular or exceeds its byte limit"
    );
    ensure!(
        !private || metadata.mode().trailing_zeros() >= 6,
        "worker private file permissions must exclude group and others"
    );
    Ok(file)
}

pub(super) fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_regular(path, true, limit)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= limit,
        "worker private file grew past its byte limit"
    );
    Ok(bytes)
}

struct HashedWriter<W> {
    writer: W,
    digest: Sha256,
}

impl<W: Write> Write for HashedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.writer.write(bytes)?;
        let written = bytes
            .get(..count)
            .ok_or_else(|| std::io::Error::other("invalid file write length"))?;
        self.digest.update(written);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn hash_copy(path: &Path, writer: impl Write) -> Result<String> {
    let mut hashed = HashedWriter {
        writer,
        digest: Sha256::new(),
    };
    let count = std::io::copy(
        &mut open_regular(path, false, MAX_BINARY)?.take(MAX_BINARY.saturating_add(1)),
        &mut hashed,
    )?;
    ensure!(count <= MAX_BINARY, "worker executable grew past 512 MiB");
    hashed.flush()?;
    Ok(format!("{:x}", hashed.digest.finalize()))
}

pub(super) fn digest(path: &Path) -> Result<String> {
    hash_copy(path, std::io::sink())
}

pub(super) fn verify_executable(path: &Path, expected: &str) -> Result<()> {
    let file = open_regular(path, true, MAX_BINARY)?;
    ensure!(
        file.metadata()?.mode() & 0o700 == 0o700,
        "immutable executable lacks owner execution permissions"
    );
    ensure!(
        digest(path)? == expected,
        "immutable executable digest mismatch"
    );
    Ok(())
}

pub(super) fn directory(path: &Path, private: bool) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "worker directory must not be a symlink"
    );
    ensure!(
        !private || metadata.mode().trailing_zeros() >= 6,
        "worker runtime directories must be private"
    );
    Ok(())
}

fn staged(path: &Path, mode: u32, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let parent = path.parent().context("worker file has no parent")?;
    let temporary = parent.join(format!(
        ".worker-stage-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    let result = (|| {
        write(&mut file)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path).context("publishing immutable worker file")?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    drop(file);
    fs::remove_file(&temporary).context("removing owned worker staging file")?;
    result
}

pub(super) fn executable(source: &Path, destination: &Path, expected: &str) -> Result<()> {
    if destination.try_exists()? {
        return verify_executable(destination, expected);
    }
    staged(destination, 0o700, |file| {
        ensure!(
            hash_copy(source, file)? == expected,
            "source executable changed during deployment"
        );
        Ok(())
    })
}

pub(super) fn immutable_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.try_exists()? {
        ensure!(
            read_private(path, 65_536)? == bytes,
            "existing immutable worker configuration differs"
        );
        return Ok(());
    }
    staged(path, 0o600, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}
