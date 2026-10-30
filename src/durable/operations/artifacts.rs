#[path = "artifacts_stream.rs"]
mod stream;

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::DirBuilderExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{OperationResult, OperationSpec};

pub(super) const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    root: PathBuf,
    directory: PathBuf,
    binding: String,
    operation_id: String,
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "operation artifact root must be absolute"
    );
    ensure!(
        !path.components().any(|part| part == Component::ParentDir),
        "operation artifact root must not traverse parents"
    );
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "operation artifact directory must not be a symlink"
    );
    path.canonicalize()
        .context("resolving operation artifact root")
}

fn read_limited(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "operation artifact must not be a symlink"
    );
    let file = File::open(path).context("opening operation artifact")?;
    ensure!(
        file.metadata()?.is_file(),
        "operation artifact must be a regular file"
    );
    ensure!(
        file.metadata()?.len() <= MAX_ARTIFACT_BYTES,
        "operation artifact exceeds 64 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= MAX_ARTIFACT_BYTES,
        "operation artifact exceeds 64 MiB"
    );
    Ok(bytes)
}

fn describe(path: &Path, bytes: &[u8]) -> Result<Artifact> {
    Ok(Artifact {
        path: path.to_path_buf(),
        sha256: digest(bytes),
        bytes: u64::try_from(bytes.len())?,
    })
}

fn publish(path: &Path, bytes: &[u8]) -> Result<Artifact> {
    ensure!(
        u64::try_from(bytes.len())? <= MAX_ARTIFACT_BYTES,
        "operation artifact exceeds 64 MiB"
    );
    if path.try_exists()? {
        ensure!(
            read_limited(path)? == bytes,
            "operation artifact binding/hash differs from existing bytes"
        );
        return describe(path, bytes);
    }
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let parent = path.parent().context("artifact parent")?;
    let temporary = parent.join(format!(".stage-{}-{sequence}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::hard_link(&temporary, path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
                read_limited(path)? == bytes,
                "concurrent operation artifact binding mismatch"
            ),
            Err(error) => return Err(error.into()),
        }
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let cleanup = fs::remove_file(&temporary);
    match (result, cleanup) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) => return Err(error),
        (Ok(()), Err(error)) => {
            return Err(error).context("removing operation artifact staging file");
        }
        (Err(error), Err(cleanup)) => {
            return Err(error.context(format!("artifact staging cleanup failed: {cleanup}")));
        }
    }
    describe(path, bytes)
}

pub fn stage_input(root: &Path, source: &Path) -> Result<Artifact> {
    let bytes = read_limited(source)?;
    store_input(root, &bytes)
}

pub(super) fn store_input(root: &Path, bytes: &[u8]) -> Result<Artifact> {
    let root = directory(root)?;
    let inputs = directory(&root.join("inputs"))?;
    publish(&inputs.join(format!("{}.input", digest(bytes))), bytes)
}

impl Workspace {
    pub fn open(root: &Path, operation_id: &str, spec: &OperationSpec) -> Result<Self> {
        ensure!(
            (1..=128).contains(&operation_id.len())
                && operation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
            "invalid bounded operation ID"
        );
        let root = directory(root)?;
        let directory =
            directory(&root.join(format!("operation-{}", digest(operation_id.as_bytes()))))?;
        let bytes = serde_json::to_vec(spec)?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "operation specification exceeds 1 MiB"
        );
        let binding = digest(&serde_json::to_vec(&(operation_id, spec))?);
        publish(&directory.join("specification.json"), &bytes)
            .context("operation input binding mismatch")?;
        Ok(Self {
            root,
            directory,
            binding,
            operation_id: operation_id.to_owned(),
        })
    }

    #[must_use]
    pub fn binding(&self) -> &str {
        &self.binding
    }

    pub fn operation_key(&self) -> Result<String> {
        Ok(format!(
            "native:{}",
            self.directory
                .file_name()
                .and_then(|name| name.to_str())
                .context("operation directory identity")?
        ))
    }

    pub fn validate_spec(&self, spec: &OperationSpec) -> Result<()> {
        ensure!(
            digest(&serde_json::to_vec(&(&self.operation_id, spec))?) == self.binding,
            "operation input binding mismatch"
        );
        Ok(())
    }

    pub(in crate::durable) fn root(&self) -> &Path {
        &self.root
    }

    pub fn complete(
        &self,
        result: &serde_json::Value,
        output: Option<&Artifact>,
    ) -> Result<OperationResult> {
        if let Some(artifact) = output {
            if artifact
                .path
                .extension()
                .is_some_and(|extension| extension == "xlsx")
            {
                self.verify_workbook(artifact)?;
            } else {
                self.read(artifact)?;
            }
        }
        let bytes = serde_json::to_vec(result)?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "operation completion receipt exceeds 1 MiB"
        );
        Ok(OperationResult {
            operation_id: self.operation_id.clone(),
            binding: self.binding.clone(),
            receipt: self.store("operation-complete.json", &bytes)?,
            output: output.map(|artifact| artifact.path.clone()),
        })
    }

    pub fn path(&self, name: &str) -> Result<PathBuf> {
        ensure!(
            (1..=128).contains(&name.len())
                && name != "."
                && name != ".."
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
            "invalid operation artifact file name"
        );
        Ok(self.directory.join(name))
    }

    pub fn store(&self, name: &str, bytes: &[u8]) -> Result<Artifact> {
        publish(&self.path(name)?, bytes)
    }

    pub fn inspect(&self, name: &str) -> Result<Artifact> {
        let path = self.path(name)?;
        describe(&path, &read_limited(&path)?)
    }

    pub fn read(&self, artifact: &Artifact) -> Result<Vec<u8>> {
        ensure!(
            artifact.path.is_absolute(),
            "operation artifact path must be absolute"
        );
        let path = artifact
            .path
            .canonicalize()
            .context("resolving operation artifact")?;
        ensure!(
            path.starts_with(&self.root),
            "operation artifact is outside the approved root"
        );
        ensure!(
            !fs::symlink_metadata(&artifact.path)?
                .file_type()
                .is_symlink(),
            "operation artifact must not be a symlink"
        );
        let bytes = read_limited(&path)?;
        ensure!(
            artifact.bytes == u64::try_from(bytes.len())? && artifact.sha256 == digest(&bytes),
            "operation artifact hash/length mismatch"
        );
        Ok(bytes)
    }

    pub(super) fn load<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.path(name)?;
        if !path.try_exists()? {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&read_limited(&path)?)?))
    }

    pub(super) fn record<T: Serialize>(&self, name: &str, value: &T) -> Result<Artifact> {
        self.store(name, &serde_json::to_vec(value)?)
    }
}
