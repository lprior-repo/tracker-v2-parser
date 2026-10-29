use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::{Artifact, Workspace};
use crate::export::MAX_EXPORT_BYTES;

struct HashingWriter<'a, W> {
    writer: &'a mut W,
    digest: Sha256,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(bytes)?;
        let accepted = bytes.get(..written).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "writer accepted more bytes than supplied",
            )
        })?;
        self.digest.update(accepted);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn stream_file(
    root: &Path,
    path: &Path,
    limit: u64,
    output: &mut impl Write,
    expected_bytes: Option<u64>,
) -> Result<Artifact> {
    ensure!(
        path.is_absolute(),
        "operation artifact path must be absolute"
    );
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "operation artifact must not be a symlink"
    );
    let canonical = path
        .canonicalize()
        .context("resolving operation artifact")?;
    ensure!(
        canonical.starts_with(root.canonicalize()?),
        "operation artifact is outside the approved root"
    );
    let descriptor = rustix::fs::open(
        &canonical,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )?;
    let mut file = File::from(descriptor);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "operation artifact must be a regular file"
    );
    ensure!(
        metadata.len() <= limit,
        "operation artifact exceeds streaming capacity"
    );
    ensure!(
        expected_bytes.is_none_or(|bytes| bytes == metadata.len()),
        "operation artifact hash/length mismatch"
    );
    let mut writer = HashingWriter {
        writer: output,
        digest: Sha256::new(),
    };
    let bytes = io::copy(&mut (&mut file).take(limit.saturating_add(1)), &mut writer)?;
    ensure!(
        bytes <= limit && bytes == metadata.len() && bytes == file.metadata()?.len(),
        "operation artifact length changed or exceeded streaming capacity"
    );
    Ok(Artifact {
        path: path.to_path_buf(),
        sha256: format!("{:x}", writer.digest.finalize()),
        bytes,
    })
}

impl Artifact {
    pub(crate) fn stream_to(&self, root: &Path, limit: u64, output: &mut impl Write) -> Result<()> {
        ensure!(
            self.bytes <= limit,
            "operation artifact exceeds streaming capacity"
        );
        ensure!(
            stream_file(root, &self.path, limit, output, Some(self.bytes))? == *self,
            "operation artifact hash/length mismatch"
        );
        Ok(())
    }
}

impl Workspace {
    pub fn inspect_workbook(&self, name: &str) -> Result<Artifact> {
        let path = self.path(name)?;
        ensure!(
            path.extension()
                .is_some_and(|extension| extension == "xlsx"),
            "streamed workbook requires an xlsx artifact"
        );
        stream_file(self.root(), &path, MAX_EXPORT_BYTES, &mut io::sink(), None)
    }

    pub fn verify_workbook(&self, artifact: &Artifact) -> Result<()> {
        ensure!(
            artifact
                .path
                .extension()
                .is_some_and(|extension| extension == "xlsx"),
            "streamed workbook requires an xlsx artifact"
        );
        artifact.stream_to(self.root(), MAX_EXPORT_BYTES, &mut io::sink())
    }
}
