use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

pub(super) const JSON_LIMIT: u64 = 4 * 1024 * 1024;

pub(super) fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string {pointer}"))
}

pub(super) fn array<'a>(value: &'a Value, pointer: &str) -> Result<&'a Vec<Value>> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .with_context(|| format!("missing array {pointer}"))
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn file_digest(path: &Path, limit: u64) -> Result<(u64, String)> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= limit,
        "proof file must be bounded and regular"
    );
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 8_192];
    let mut consumed = 0_u64;
    for _ in 0..=metadata.len().div_ceil(8_192) {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        consumed = consumed
            .checked_add(u64::try_from(count)?)
            .context("proof file size overflow")?;
        ensure!(consumed <= metadata.len(), "proof file grew during hashing");
        hash.update(buffer.get(..count).context("proof read size invalid")?);
    }
    ensure!(
        consumed == metadata.len() && file.metadata()?.len() == consumed,
        "proof file changed during hashing"
    );
    Ok((consumed, format!("{:x}", hash.finalize())))
}

pub(super) fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit.checked_add(1).context("proof read limit overflow")?)
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= limit,
        "proof input exceeds bound"
    );
    Ok(bytes)
}

pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn save(path: &Path, value: &Value) -> Result<()> {
    write(path, &serde_json::to_vec_pretty(value)?)
}

pub(super) fn artifact(value: &Value, root: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(text(value, "/path")?);
    ensure!(
        path.canonicalize()?.starts_with(root.canonicalize()?),
        "artifact escaped owned root"
    );
    let (bytes, sha) = file_digest(&path, crate::export::MAX_EXPORT_BYTES)?;
    ensure!(
        value.get("bytes").and_then(Value::as_u64) == Some(bytes) && text(value, "/sha256")? == sha,
        "artifact hash or byte count mismatch"
    );
    Ok(path)
}

pub(super) struct Http {
    client: reqwest::Client,
}

impl Http {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(10))
                .build()?,
        })
    }

    pub async fn request(
        &self,
        base: &str,
        route: &str,
        value: Option<&Value>,
        key: Option<&str>,
    ) -> Result<(u16, Value)> {
        let url = url::Url::parse(base)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str().is_some_and(|host| host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())),
            "proof HTTP must use loopback"
        );
        let request = value
            .map_or_else(
                || self.client.get(format!("{base}{route}")),
                |value| self.client.post(format!("{base}{route}")).json(value),
            )
            .header(reqwest::header::ACCEPT, "application/json");
        let request = match key {
            Some(key) => request.header("idempotency-key", key),
            None => request,
        };
        let response = request.send().await?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes_stream()
            .map_err(anyhow::Error::from)
            .try_fold(Vec::new(), |mut bytes, chunk| async move {
                ensure!(
                    u64::try_from(bytes.len().saturating_add(chunk.len()))? <= JSON_LIMIT,
                    "proof HTTP response exceeds 4 MiB"
                );
                bytes.extend_from_slice(&chunk);
                Ok::<_, anyhow::Error>(bytes)
            })
            .await?;
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        Ok((status, body))
    }

    pub async fn health(&self, base: &str, route: &str) -> Result<()> {
        let deadline = deadline(Duration::from_secs(90))?;
        let ready = stream::iter(0..360)
            .fold(false, |ready, _| async move {
                if ready {
                    return true;
                }
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                if self
                    .request(base, route, None, None)
                    .await
                    .is_ok_and(|(status, _)| status == 200)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
                false
            })
            .await;
        ensure!(ready, "proof health route {route} did not become ready");
        Ok(())
    }

    pub async fn submit(&self, base: &str, value: &Value) -> Result<(String, Value)> {
        let (status, result) = self
            .request(
                base,
                "/restate/send/AthleticOperations/execute",
                Some(value),
                Some(text(value, "/operation_id")?),
            )
            .await?;
        ensure!(
            matches!(status, 200 | 202),
            "operation submission failed HTTP {status}"
        );
        Ok((invocation(&result)?.to_owned(), result))
    }

    pub async fn output(&self, base: &str, id: &str, field: &str) -> Result<Value> {
        let deadline = deadline(Duration::from_mins(3))?;
        stream::iter(0..720)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(None, |found, _| async move {
                if found.is_some() {
                    return Ok(found);
                }
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "native output exceeded180second deadline"
                );
                if let Ok((status, value)) = self
                    .request(base, &format!("/restate/output/{id}"), None, None)
                    .await
                {
                    if status == 200 && value.get(field).is_some() {
                        return Ok(Some(value));
                    }
                    ensure!(
                        matches!(status, 200 | 202 | 404 | 409 | 470 | 503),
                        "invocation output failed HTTP {status}"
                    );
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(None)
            })
            .await?
            .context("native invocation output exceeded bounded wait")
    }
}

pub(super) fn invocation(value: &Value) -> Result<&str> {
    ["invocationId", "invocation_id", "id"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|id| id.starts_with("inv_"))
        .context("missing invocation ID")
}

pub(super) fn deadline(duration: Duration) -> Result<tokio::time::Instant> {
    tokio::time::Instant::now()
        .checked_add(duration)
        .context("proof deadline overflow")
}
