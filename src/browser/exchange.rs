use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{
        fetch::{EventRequestPaused, HeaderEntry},
        network::{GetRequestPostDataParams, Request},
    },
};
use futures::TryStreamExt;
use reqwest::{Client, Method, header};
use url::Url;

use crate::{
    model::Target,
    transport::{AccessStopCaptureError, MAX_BODY_BYTES, access_stop_status, denied_url, policy},
};

pub(super) struct DestinationPolicy {
    pacing: super::pacing::Pacing,
    target: Target,
    caller_headers: BTreeSet<String>,
    #[cfg(test)]
    local_origins: Vec<url::Origin>,
}

pub(super) struct Response {
    pub status: u16,
    pub headers: Vec<HeaderEntry>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct RejectedDestination(anyhow::Error);

impl std::fmt::Display for RejectedDestination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "browser destination rejected: {:#}", self.0)
    }
}

impl std::error::Error for RejectedDestination {}

impl DestinationPolicy {
    pub fn new(target: &Target, headers: &BTreeMap<String, String>, interval: Duration) -> Self {
        Self {
            pacing: super::pacing::Pacing::new(interval),
            target: target.clone(),
            caller_headers: headers
                .keys()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            #[cfg(test)]
            local_origins: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn allow_local_origin(&mut self, origin: url::Origin) {
        self.local_origins.push(origin);
    }

    pub fn check_stop(&self) -> Result<()> {
        self.pacing.check_stop()
    }

    fn url(&self, address: &str) -> Result<Url> {
        if denied_url(address) {
            let url = [address, self.target.url.as_str()]
                .into_iter()
                .filter_map(|value| Url::parse(value).ok())
                .find(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .context("access-stop has no HTTP authority")?;
            return Err(crate::transport::AccessStop {
                origin: url.origin().ascii_serialization(),
                status: None,
            }
            .into());
        }
        self.checked_url(address)
            .map_err(|error| anyhow::Error::new(RejectedDestination(error)))
    }

    fn checked_url(&self, address: &str) -> Result<Url> {
        let url = Url::parse(address).context("parsing browser destination")?;
        ensure!(
            !denied_url(address),
            "browser destination requires human access review"
        );
        ensure!(
            !self.target.url.starts_with("https:") || url.scheme() == "https",
            "refusing browser HTTPS downgrade"
        );
        #[cfg(test)]
        if self.local_origins.contains(&url.origin()) {
            ensure!(
                url.username().is_empty() && url.password().is_none(),
                "browser destination contains credentials"
            );
            policy::check_url_secrets(&url)?;
            return Ok(url);
        }
        policy::source_url(&self.target, address)
    }

    async fn addresses(&self, url: &Url) -> Result<Vec<SocketAddr>> {
        #[cfg(test)]
        if self.local_origins.contains(&url.origin()) {
            return Ok(vec![SocketAddr::new(
                "127.0.0.1".parse()?,
                url.port_or_known_default()
                    .context("local fixture has no port")?,
            )]);
        }
        policy::public_addresses(url).await
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn request(
    policy: &DestinationPolicy,
    page: &Page,
    event: &EventRequestPaused,
) -> Result<Response> {
    let request = &event.request;
    let url = policy.url(&request.url)?;
    let addresses = tokio::time::timeout(super::COMMAND_TIMEOUT, policy.addresses(&url))
        .await
        .context("browser DNS resolution timed out")??;
    let host = url.host_str().context("browser destination has no host")?;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .connect_timeout(Duration::from_secs(15))
        .resolve_to_addrs(host, &addresses)
        .build()
        .context("building pinned browser request")?;
    let method = Method::from_bytes(request.method.as_bytes())?;
    ensure!(
        matches!(
            method,
            Method::GET | Method::POST | Method::HEAD | Method::OPTIONS
        ),
        "unsupported browser method"
    );
    let headers = request_headers(policy, request, &url)?;
    let body = request_body(page, event).await?;
    policy.pacing.admit().await?;
    let response = client
        .request(method, url.clone())
        .headers(headers)
        .body(body)
        .send()
        .await?;
    if access_stop_status(response.status().as_u16()) {
        policy.pacing.stop(
            url.origin().ascii_serialization(),
            response.status().as_u16(),
        )?;
    }
    capture(policy, request, response).await
}

fn request_headers(
    policy: &DestinationPolicy,
    request: &Request,
    url: &Url,
) -> Result<header::HeaderMap> {
    let same_origin = Url::parse(&policy.target.url)?.origin() == url.origin();
    request
        .headers
        .inner()
        .as_object()
        .context("browser request headers are not an object")?
        .iter()
        .try_fold(header::HeaderMap::new(), |mut headers, (name, value)| {
            let name = header::HeaderName::from_bytes(name.as_bytes())?;
            if matches!(
                name.as_str(),
                "host" | "content-length" | "connection" | "accept-encoding"
            ) {
                return Ok(headers);
            }
            ensure!(
                same_origin
                    || (!policy.caller_headers.contains(name.as_str())
                        && browser_managed_header(name.as_str())),
                "refusing origin-bound browser header on another origin"
            );
            let value = value.as_str().context("browser header is not a string")?;
            if !same_origin && name.as_str() == "access-control-request-headers" {
                ensure!(
                    value.split(',').map(str::trim).all(|name| {
                        let name = name.to_ascii_lowercase();
                        browser_managed_header(&name) && !policy.caller_headers.contains(&name)
                    }),
                    "refusing cross-origin browser credential preflight"
                );
            }
            headers.append(name, header::HeaderValue::from_str(value)?);
            Ok(headers)
        })
}

async fn request_body(page: &Page, event: &EventRequestPaused) -> Result<Vec<u8>> {
    let request = &event.request;
    if request.has_post_data != Some(true) {
        return Ok(Vec::new());
    }
    let Some(entries) = &request.post_data_entries else {
        let id = event
            .network_id
            .clone()
            .context("browser request body has no network identifier")?;
        let body = page
            .execute(GetRequestPostDataParams::new(id))
            .await?
            .result
            .post_data;
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "browser request body exceeds limit"
        );
        ensure!(
            !request
                .headers
                .inner()
                .as_object()
                .is_some_and(|headers| headers.iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        && value
                            .as_str()
                            .is_some_and(|value| value.starts_with("multipart/"))
                })),
            "browser multipart body entries were omitted"
        );
        return Ok(body.into_bytes());
    };
    entries.iter().try_fold(Vec::new(), |mut body, entry| {
        let bytes = entry
            .bytes
            .as_ref()
            .context("browser request body entry was omitted")?;
        let bytes = super::decode_body(bytes.as_ref())?;
        ensure!(
            body.len().saturating_add(bytes.len()) <= MAX_BODY_BYTES,
            "browser request body exceeds limit"
        );
        body.try_reserve(bytes.len())?;
        body.extend_from_slice(&bytes);
        Ok(body)
    })
}

async fn capture(
    policy: &DestinationPolicy,
    request: &Request,
    response: reqwest::Response,
) -> Result<Response> {
    let status = response.status().as_u16();
    let address = response.url().as_str().to_owned();
    capture_response(policy, request, response)
        .await
        .map_err(|error| {
            if access_stop_status(status) {
                AccessStopCaptureError::new(status, error, &address).into()
            } else {
                error
            }
        })
}

async fn capture_response(
    policy: &DestinationPolicy,
    request: &Request,
    response: reqwest::Response,
) -> Result<Response> {
    let status = response.status().as_u16();
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        let location = response
            .headers()
            .get(header::LOCATION)
            .context("browser redirect omitted Location")?
            .to_str()?;
        let next = response.url().join(location)?;
        policy.url(next.as_str())?;
        ensure!(
            response.url().scheme() != "https" || next.scheme() == "https",
            "refusing browser HTTPS downgrade"
        );
        ensure!(
            request.method == "GET" || request.method == "HEAD",
            "refusing to replay redirected browser request body"
        );
        ensure!(
            !has_credentials(request) || response.url().origin() == next.origin(),
            "refusing authenticated browser redirect to another origin"
        );
    }
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= 16_777_216),
        "browser response exceeded 16 MiB limit"
    );
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "content-length" | "transfer-encoding" | "connection"
            )
        })
        .map(|(name, value)| Ok(HeaderEntry::new(name.as_str(), value.to_str()?)))
        .collect::<Result<Vec<_>>>()?;
    let body = response
        .bytes_stream()
        .map_err(anyhow::Error::from)
        .try_fold(Vec::new(), |mut body, chunk| async move {
            ensure!(
                body.len().saturating_add(chunk.len()) <= MAX_BODY_BYTES,
                "browser response exceeded 16 MiB limit"
            );
            body.try_reserve(chunk.len())?;
            body.extend_from_slice(&chunk);
            Ok(body)
        })
        .await?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

fn has_credentials(request: &Request) -> bool {
    request.headers.inner().as_object().is_some_and(|headers| {
        headers.keys().any(|name| {
            !browser_managed_header(&name.to_ascii_lowercase())
                || name.eq_ignore_ascii_case("cookie")
        })
    })
}

fn browser_managed_header(name: &str) -> bool {
    name.starts_with("sec-")
        || matches!(
            name,
            "accept"
                | "accept-language"
                | "accept-encoding"
                | "user-agent"
                | "referer"
                | "origin"
                | "connection"
                | "host"
                | "cache-control"
                | "pragma"
                | "content-type"
                | "content-length"
                | "range"
                | "priority"
                | "cookie"
                | "access-control-request-method"
                | "access-control-request-headers"
                | "upgrade-insecure-requests"
                | "purpose"
                | "save-data"
                | "content-language"
                | "accept-charset"
                | "if-match"
                | "if-none-match"
                | "if-modified-since"
                | "if-unmodified-since"
                | "if-range"
        )
}

pub(super) fn encoded_body(body: &[u8]) -> String {
    STANDARD.encode(body)
}
