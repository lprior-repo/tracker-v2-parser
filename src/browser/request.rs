use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::model::{Capture, Target};

use super::MAX_BODY_BYTES;

pub(super) struct RequestContext {
    pub(super) target: Url,
    pub(super) navigation: Url,
    pub(super) origin: String,
}

impl RequestContext {
    pub(super) fn parse(target: &Target) -> Result<Self> {
        let url = source_url(&target.url)?;
        let navigation = match &target.browser_origin {
            Some(origin) => source_url(origin)?,
            None => source_url(&url.origin().ascii_serialization())?,
        };
        ensure!(
            url.host_str() == navigation.host_str(),
            "browser target host must equal browser context host"
        );
        let origin = navigation.origin().ascii_serialization();
        Ok(Self {
            target: url,
            navigation,
            origin,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct PageState {
    url: String,
    denial: Option<String>,
}

impl PageState {
    pub(super) fn validate(&self, context: &RequestContext) -> Result<()> {
        if let Some(reason) = &self.denial {
            return denial_error(reason);
        }
        let actual = source_url(&self.url)?;
        ensure!(
            actual.origin().ascii_serialization() == context.origin,
            "browser navigation left the required origin; authentication/redirect requires human review"
        );
        ensure!(
            !login_path(&actual),
            "browser navigation reached a login page; authentication requires human review"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
pub(super) struct FetchReply {
    error: Option<String>,
    status: Option<u16>,
    content_type: Option<String>,
    url: Option<String>,
    body: Option<String>,
}

impl FetchReply {
    pub(super) fn into_capture(self, context: &RequestContext) -> Result<Capture> {
        if let Some(error) = self.error {
            return fetch_error(&error);
        }
        let status = self
            .status
            .context("browser fetch did not return an HTTP status")?;
        ensure!(
            (100..=599).contains(&status),
            "browser fetch returned an unreadable HTTP status"
        );
        let url = self
            .url
            .context("browser fetch did not return a final URL")?;
        let actual = source_url(&url)?;
        ensure!(
            actual.host_str() == context.target.host_str(),
            "browser fetch returned a different response host"
        );
        let encoded = self
            .body
            .context("browser fetch did not return response bytes")?;
        ensure!(
            encoded.len() <= MAX_BODY_BYTES.div_ceil(3).saturating_mul(4),
            "browser response exceeded 16 MiB limit"
        );
        let body = STANDARD
            .decode(encoded)
            .map_err(|_| anyhow!("browser fetch returned invalid byte encoding"))?;
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "browser response exceeded 16 MiB limit"
        );
        Ok(Capture {
            url,
            status,
            content_type: self
                .content_type
                .context("browser fetch did not return content type")?,
            body,
        })
    }
}

pub(super) fn fetch_arguments(
    target: &Target,
    headers: &BTreeMap<String, String>,
    context: &RequestContext,
) -> Result<Value> {
    let method = target.method.as_deref().map_or("GET", str::trim);
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| anyhow!("invalid browser request method"))?;
    let body = target
        .body
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("encoding browser request JSON")?;
    if let Some(body) = &body {
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "browser request exceeded 16 MiB limit"
        );
        ensure!(
            method != reqwest::Method::GET && method != reqwest::Method::HEAD,
            "GET/HEAD browser requests cannot have a body"
        );
    }
    ensure!(
        headers.len() <= 128,
        "browser request exceeded header count limit"
    );
    headers.iter().try_for_each(|(name, value)| {
        reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("invalid browser request header name"))?;
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| anyhow!("invalid browser request header value"))?;
        ensure!(
            name.len().saturating_add(value.len()) <= 65_536,
            "browser request header exceeded size limit"
        );
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(json!({
        "url": context.target.as_str(),
        "origin": context.origin,
        "method": method.as_str(),
        "headers": headers,
        "body": body,
        "max_bytes": MAX_BODY_BYTES,
        "max_chunks": 4096
    }))
}

fn source_url(address: &str) -> Result<Url> {
    let url = Url::parse(address).map_err(|_| anyhow!("invalid browser source URL"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "browser source URL must use HTTP or HTTPS with a host"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "browser source URL must not contain credentials"
    );
    Ok(url)
}

fn login_path(url: &Url) -> bool {
    url.path_segments().is_some_and(|segments| {
        segments.take(4).any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "login" | "log-in" | "signin" | "sign-in"
            )
        })
    })
}

fn denial_error(reason: &str) -> Result<()> {
    match reason {
        "challenge" => {
            bail!("browser navigation encountered an access challenge; human handling required")
        }
        "authentication" => {
            bail!("browser navigation requires authentication; human handling required")
        }
        "paywall" => bail!("browser navigation encountered a paywall; human handling required"),
        _ => bail!("browser navigation was denied; human handling required"),
    }
}

fn fetch_error(error: &str) -> Result<Capture> {
    match error {
        "timeout" => bail!("browser fetch exceeded 30 second deadline"),
        "size" => bail!("browser response exceeded 16 MiB limit"),
        "chunks" => bail!("browser response exceeded 4096 chunk limit"),
        "origin" => bail!("browser page no longer matches the required origin"),
        "host" => bail!("browser request host differs from its context host"),
        "unreadable" => bail!("browser fetch returned an opaque or unreadable response"),
        "network" => {
            bail!("browser fetch failed: network, CORS, or redirect rejected (no retry attempted)")
        }
        _ => bail!("browser fetch failed before a readable response was available"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{FetchReply, PageState, RequestContext, fetch_arguments};
    use crate::model::{PageContext, PageKind, Source, Target};

    fn target(url: &str) -> Target {
        Target::get(
            Source::AthleticNet,
            PageKind::Athlete,
            url.to_owned(),
            PageContext::default(),
        )
    }

    #[test]
    fn given_context_host_when_target_differs_then_request_is_rejected() {
        let mut target = target("https://api.example.test/results");
        target.browser_origin = Some("https://www.example.test/".to_owned());
        assert!(RequestContext::parse(&target).is_err());
        target.browser_origin = Some("https://api.example.test/home".to_owned());
        assert!(RequestContext::parse(&target).is_ok());
    }

    #[test]
    fn given_response_bytes_when_captured_then_status_and_non_utf8_bytes_survive() {
        let context = RequestContext::parse(&target("https://example.test/api")).expect("context");
        let reply: FetchReply = serde_json::from_value(serde_json::json!({
            "status": 403, "content_type": "application/octet-stream",
            "url": "https://example.test/api", "body": "AP+A"
        }))
        .expect("reply");
        let capture = reply
            .into_capture(&context)
            .expect("real HTTP error response");
        assert_eq!(capture.status, 403);
        assert_eq!(capture.body, vec![0, 255, 128]);
    }

    #[test]
    fn given_navigation_when_login_or_challenge_then_no_api_fetch_is_allowed() {
        let context = RequestContext::parse(&target("https://example.test/api")).expect("context");
        assert!(
            PageState {
                url: "https://example.test/login".to_owned(),
                denial: None
            }
            .validate(&context)
            .is_err()
        );
        assert!(
            PageState {
                url: "https://example.test/".to_owned(),
                denial: Some("challenge".to_owned())
            }
            .validate(&context)
            .is_err()
        );
        assert!(
            PageState {
                url: "https://example.test/".to_owned(),
                denial: None
            }
            .validate(&context)
            .is_ok()
        );
    }

    #[test]
    fn given_token_preflight_field_when_arguments_built_then_only_explicit_headers_are_forwarded() {
        let mut target = target("https://example.test/api");
        target.token_url = Some("https://example.test/token".to_owned());
        let context = RequestContext::parse(&target).expect("context");
        let arguments = fetch_arguments(&target, &BTreeMap::new(), &context).expect("arguments");
        assert_eq!(arguments["url"], "https://example.test/api");
        assert_eq!(arguments["headers"], serde_json::json!({}));
        assert!(arguments.get("token_url").is_none());
    }
}
