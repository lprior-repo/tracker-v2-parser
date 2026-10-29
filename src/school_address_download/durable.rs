use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use reqwest::{Client, Response, header};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use url::Url;

use crate::{
    durable::pacing::{PersistentPacing, WireRequest},
    model::Source,
};

#[derive(Debug)]
pub struct DownloadTransient {
    status: u16,
}

impl std::fmt::Display for DownloadTransient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "school reference source temporarily unavailable (HTTP {})",
            self.status
        )
    }
}

impl std::error::Error for DownloadTransient {}

enum Hop {
    Next(Url),
    Complete(Response),
}

#[cfg(test)]
mod tests;

fn approved(url: &Url) -> Result<()> {
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("raw.githubusercontent.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url.fragment().is_none(),
        "address download redirect is outside the approved source"
    );
    Ok(())
}

fn attempt_identity(operation_id: &str, expected: &str, ordinal: u8) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            operation_id,
            expected.to_ascii_lowercase(),
            ordinal
        ))?)
    ))
}

async fn next_attempt(pool: &PgPool, operation_id: &str, expected: &str) -> Result<String> {
    let allowed = (1..=3)
        .map(|ordinal| attempt_identity(operation_id, expected, ordinal))
        .collect::<Result<Vec<_>>>()?;
    let used: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT attempt_id FROM durable_wire_requests WHERE operation_id=$1 LIMIT 4",
    )
    .bind(operation_id)
    .fetch_all(pool)
    .await?;
    ensure!(
        used.iter().all(|id| allowed.contains(id)),
        "reference operation has incompatible wire history"
    );
    allowed
        .into_iter()
        .find(|id| !used.contains(id))
        .context("school address download exhausted three persisted source attempts")
}

async fn hop(client: &Client, pacing: &PersistentPacing, url: Url) -> Result<Hop> {
    approved(&url)?;
    let identity = WireRequest::new(Source::MileSplit, url.as_str(), "GET", &[])?;
    let response = pacing
        .send(identity, Duration::from_secs(5), async {
            client
                .get(url.clone())
                .send()
                .await
                .context("fetching school address CSV")
        })
        .await?;
    let status = response.status().as_u16();
    if crate::transport::access_stop_status(status) {
        return Err(crate::transport::AccessStop {
            origin: url.origin().ascii_serialization(),
            status: Some(status),
        }
        .into());
    }
    if matches!(status, 408 | 425 | 500 | 502 | 503 | 504) {
        return Err(DownloadTransient { status }.into());
    }
    if response.status().is_redirection() {
        let next = response
            .headers()
            .get(header::LOCATION)
            .context("address redirect omitted Location")?
            .to_str()?;
        let next = url
            .join(next)
            .context("resolving address download redirect")?;
        approved(&next)?;
        return Ok(Hop::Next(next));
    }
    Ok(Hop::Complete(response))
}

async fn fetch(pool: PgPool, operation_id: &str, expected: &str) -> Result<Vec<u8>> {
    let attempt = next_attempt(&pool, operation_id, expected).await?;
    let pacing = PersistentPacing::new(pool).with_wire_context(operation_id, &attempt)?;
    let client = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_mins(2))
        .connect_timeout(Duration::from_secs(15))
        .user_agent("track-scrapper-v2/0.1 public athletics research")
        .build()?;
    let result = stream::iter(0..6)
        .map(Ok::<_, anyhow::Error>)
        .try_fold(Hop::Next(Url::parse(super::SOURCE)?), |state, _| async {
            match state {
                Hop::Next(url) => hop(&client, &pacing, url).await,
                Hop::Complete(response) => Ok(Hop::Complete(response)),
            }
        })
        .await?;
    let response = match result {
        Hop::Complete(response) => response,
        Hop::Next(_) => return Err(anyhow!("address download exceeded five redirect limit")),
    };
    super::response_bytes(response).await
}

#[tracing::instrument(skip_all)]
pub async fn download_durable(
    expected_sha256: &str,
    output_path: &Path,
    pool: PgPool,
    operation_id: &str,
) -> Result<()> {
    super::validate_digest(expected_sha256)?;
    let bytes = fetch(pool, operation_id, expected_sha256).await?;
    ensure!(!bytes.is_empty(), "school address download was empty");
    let expected = expected_sha256.to_owned();
    let path = PathBuf::from(output_path);
    tokio::task::spawn_blocking(move || super::save_verified(&bytes, &expected, &path))
        .await
        .context("school address file writer failed")?
}
