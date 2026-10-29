use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use futures::TryStreamExt;
use reqwest::Response;
use sha2::{Digest, Sha256};

mod durable;

pub use durable::{DownloadTransient, download_durable};

pub const SOURCE: &str = "https://raw.githubusercontent.com/lprior-repo/rust-athletic-school-addresses/b7058732f05ab59105aa042e66d8a8cad467bf3c/output/all_us_high_schools.csv";
const MAX_BYTES: usize = 64 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn validate_digest(expected: &str) -> Result<()> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "expected school address SHA-256 must be 64 hexadecimal characters"
    );
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn response_bytes(response: Response) -> Result<Vec<u8>> {
    let response = response
        .error_for_status()
        .context("school address download HTTP error")?;
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= 67_108_864),
        "school address download exceeds 64 MiB"
    );
    response
        .bytes_stream()
        .map_err(anyhow::Error::from)
        .try_fold(Vec::new(), |mut bytes, chunk| async move {
            let size = bytes
                .len()
                .checked_add(chunk.len())
                .context("school address download length overflow")?;
            ensure!(size <= MAX_BYTES, "school address download exceeds 64 MiB");
            bytes
                .try_reserve(chunk.len())
                .context("allocating school address download")?;
            bytes.extend_from_slice(&chunk);
            Ok(bytes)
        })
        .await
}

fn save_verified(bytes: &[u8], expected: &str, path: &Path) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        "school address download SHA-256 mismatch: got {actual}"
    );
    let name = path
        .file_name()
        .context("school address output path requires a file name")?
        .to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("creating staged school address file")?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .context("writing staged school address file")?;
        file.sync_all()
            .context("syncing staged school address file")?;
        fs::hard_link(&temporary, path)
            .context("publishing school address file without overwrite")?;
        Ok(())
    })();
    let cleanup = fs::remove_file(&temporary).context("removing staged school address file");
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.context("school address file was published")),
        (Err(error), Err(cleanup)) => Err(error.context(cleanup.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_file_is_published_without_overwriting() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("addresses.csv");
        let bytes = b"name,address\nExample,1 Main St\n";
        let digest = format!("{:x}", Sha256::digest(bytes));
        save_verified(bytes, &digest, &path)?;
        assert_eq!(fs::read(&path)?, bytes);
        assert!(save_verified(bytes, &digest, &path).is_err());
        assert_eq!(fs::read(&path)?, bytes);
        Ok(())
    }

    #[test]
    fn mismatch_and_invalid_hash_do_not_publish() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("addresses.csv");
        assert!(validate_digest("not-a-digest").is_err());
        assert!(save_verified(b"content", &"0".repeat(64), &path).is_err());
        assert!(!path.exists());
        Ok(())
    }
}
