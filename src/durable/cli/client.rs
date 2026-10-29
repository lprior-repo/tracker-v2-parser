use anyhow::{Context, Result, ensure};
use futures::TryStreamExt;
use serde::Serialize;
use serde_json::Value;
use std::{net::IpAddr, path::Path, time::Duration};
use url::Url;

pub fn read_request<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "durable request exceeds 1 MiB");
    serde_json::from_slice(&bytes).context("decoding bounded durable request")
}

pub async fn send<T: Serialize + Sync>(
    ingress: &str,
    route: &str,
    key: Option<&str>,
    value: Option<&T>,
) -> Result<Value> {
    let base = Url::parse(ingress)?;
    ensure!(
        base.scheme() == "http"
            && base.username().is_empty()
            && base.password().is_none()
            && base.query().is_none()
            && base.fragment().is_none()
            && base.path() == "/"
            && base
                .host_str()
                .is_some_and(|host| host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())),
        "Restate ingress must be an uncredentialed loopback HTTP origin"
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let request = client.post(base.join(route)?);
    let request = match value {
        Some(value) => request.json(value),
        None => request,
    };
    let request = match key {
        Some(key) => request.header("idempotency-key", key),
        None => request,
    };
    let response = request.send().await?;
    let status = response.status();
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= 1_048_576),
        "Restate response exceeds 1 MiB"
    );
    let bytes = response
        .bytes_stream()
        .map_err(anyhow::Error::from)
        .try_fold(Vec::new(), |mut bytes, chunk| async move {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= 1_048_576,
                "Restate response exceeds 1 MiB"
            );
            bytes.extend_from_slice(&chunk);
            Ok::<_, anyhow::Error>(bytes)
        })
        .await?;
    ensure!(
        status.is_success(),
        "Restate request failed with HTTP {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).context("decoding Restate response")
}
