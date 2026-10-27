use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::model::{Capture, ParsedPage};

const MAX_BYTES: usize = 16 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
pub struct RetainedCapture {
    artifact_path: String,
    sha256: String,
    bytes: u64,
    source_url: String,
    http_status: u16,
    content_type: String,
    retained_at: chrono::DateTime<chrono::Utc>,
    representation: &'static str,
}

fn evidence_directory() -> Result<PathBuf> {
    let path = if let Some(path) = env::var_os("TRACK_SCRAPPER_EVIDENCE_DIR") {
        PathBuf::from(path)
    } else {
        let base = match env::var_os("XDG_DATA_HOME") {
            Some(base) => PathBuf::from(base),
            None => {
                PathBuf::from(env::var_os("HOME").context("HOME is required for profile evidence")?)
                    .join(".local/share")
            }
        };
        base.join("track-scrapper-v2/evidence")
    };
    ensure!(
        path.is_absolute(),
        "profile evidence directory must be absolute"
    );
    Ok(path)
}

fn verify_existing(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == u64::try_from(bytes.len())?,
        "retained profile evidence has an unexpected size"
    );
    let mut buffer = [0_u8; 8_192];
    for chunk in bytes.chunks(buffer.len()) {
        let destination = buffer
            .get_mut(..chunk.len())
            .context("invalid evidence verification chunk")?;
        file.read_exact(destination)
            .context("reading retained profile evidence")?;
        ensure!(
            destination == chunk,
            "retained profile evidence does not match its content address"
        );
    }
    Ok(())
}

fn publish(root: &Path, path: &Path, bytes: &[u8], sha256: &str) -> Result<()> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = root.join(format!(".{sha256}.{}.{}.tmp", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("creating staged profile evidence")?;
    let result = (|| -> Result<()> {
        file.write_all(bytes).context("writing profile evidence")?;
        file.sync_all().context("syncing profile evidence")?;
        match fs::hard_link(&temporary, path) {
            Ok(()) => File::open(root)?
                .sync_all()
                .context("syncing profile evidence directory"),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing(path, bytes)
            }
            Err(error) => Err(error).context("publishing profile evidence"),
        }
    })();
    let cleanup = fs::remove_file(&temporary).context("removing staged profile evidence");
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(error.context(cleanup.to_string())),
    }
}

fn store(root: &Path, capture: &Capture) -> Result<RetainedCapture> {
    ensure!(
        capture.body.len() <= MAX_BYTES,
        "profile evidence exceeds 16 MiB"
    );
    fs::create_dir_all(root).context("creating profile evidence directory")?;
    let sha256 = format!("{:x}", Sha256::digest(&capture.body));
    let path = root.join(format!("{sha256}.body"));
    publish(root, &path, &capture.body, &sha256)?;
    let path = path
        .canonicalize()
        .context("resolving retained profile evidence path")?;
    Ok(RetainedCapture {
        artifact_path: path
            .to_str()
            .context("profile evidence path is not UTF-8")?
            .to_owned(),
        sha256,
        bytes: u64::try_from(capture.body.len())?,
        source_url: capture.url.clone(),
        http_status: capture.status,
        content_type: capture.content_type.clone(),
        retained_at: chrono::Utc::now(),
        representation: "transport_sanitized_response",
    })
}

#[tracing::instrument(skip_all)]
pub async fn retain(capture: Capture) -> Result<(Capture, RetainedCapture)> {
    tokio::task::spawn_blocking(move || {
        let evidence = store(&evidence_directory()?, &capture)?;
        Ok((capture, evidence))
    })
    .await
    .context("profile evidence writer task failed")?
}

impl RetainedCapture {
    pub fn describe(&self) -> String {
        format!(
            "retained profile evidence {} at {}",
            self.sha256, self.artifact_path
        )
    }

    pub fn attach(self, page: &mut ParsedPage, requested_url: &str) -> Result<()> {
        let capture = serde_json::to_value(self)?;
        page.athletes.iter_mut().try_for_each(|athlete| {
            let values = athlete
                .source_values
                .get_or_insert_with(|| serde_json::json!({}));
            let object = values
                .as_object_mut()
                .context("athlete source values must be an object")?;
            if let Some(entry) = object
                .get_mut("pages")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|pages| pages.get_mut(requested_url))
                .and_then(serde_json::Value::as_object_mut)
            {
                entry.insert("_capture".to_owned(), capture.clone());
            }
            object.insert("_capture".to_owned(), capture.clone());
            Ok::<(), anyhow::Error>(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> Capture {
        Capture {
            url: "https://oh.milesplit.com/athletes/15005189-brice-fuller".to_owned(),
            status: 200,
            content_type: "text/html".to_owned(),
            body: b"<h1>Published athlete</h1>".to_vec(),
        }
    }

    #[test]
    fn replay_preserves_identical_content_without_replacing_corrupt_evidence() -> Result<()> {
        let root = tempfile::tempdir()?;
        let capture = capture();
        let first = store(root.path(), &capture)?;
        let second = store(root.path(), &capture)?;
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.artifact_path, second.artifact_path);
        assert_eq!(fs::read(&first.artifact_path)?, capture.body);
        fs::write(&first.artifact_path, b"corrupt")?;
        assert!(store(root.path(), &capture).is_err());
        assert_eq!(fs::read(&first.artifact_path)?, b"corrupt");
        Ok(())
    }
}
