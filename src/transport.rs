mod http;
mod policy;
mod redact;

use std::{collections::BTreeMap, fmt, path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use reqwest::Client;
use serde_json::Value;
use tokio::time::Instant;

use crate::{
    browser::Browser,
    model::{Capture, PageKind, Source, Target},
};

pub(crate) const MAX_BODY_BYTES: usize = 16_777_216;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Http,
    Browser,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub driver_url: Option<String>,
    pub profile: PathBuf,
    pub headless: bool,
    pub rps: f64,
}

#[derive(Debug)]
pub struct Failure {
    pub capture: Option<Capture>,
    pub blocked: bool,
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for Failure {}

#[derive(Debug)]
struct AccessStopCaptureError {
    status: u16,
    error: anyhow::Error,
}

impl fmt::Display for AccessStopCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source returned HTTP {}; capturing response failed: {:#}",
            self.status, self.error
        )
    }
}

impl std::error::Error for AccessStopCaptureError {}

const fn access_stop_status(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 429 | 451)
}

pub struct Transport {
    options: Options,
    browser: Option<Browser>,
    clients: BTreeMap<String, Client>,
    next_request: BTreeMap<Source, Instant>,
}

impl Transport {
    pub fn new(options: Options) -> Result<Self> {
        ensure!(
            options.rps.is_finite() && options.rps > 0.0 && options.rps <= 1.0,
            "--rps must be finite, positive, and no greater than 1"
        );
        Ok(Self {
            options,
            browser: None,
            clients: BTreeMap::new(),
            next_request: BTreeMap::new(),
        })
    }

    #[tracing::instrument(skip_all)]
    pub async fn fetch(&mut self, target: &Target) -> Result<Capture, Failure> {
        let headers = self.token_headers(target).await?;
        let response = self
            .one(target, &headers)
            .await
            .map_err(|error| failure(&error))?;
        check_response(response)
    }

    #[tracing::instrument(skip_all)]
    async fn token_headers(
        &mut self,
        target: &Target,
    ) -> Result<BTreeMap<String, String>, Failure> {
        let Some(url) = &target.token_url else {
            return Ok(BTreeMap::new());
        };
        let mut preflight = target.clone();
        preflight.url.clone_from(url);
        preflight.token_url = None;
        preflight.method = None;
        preflight.body = None;
        let response = self
            .one(&preflight, &BTreeMap::new())
            .await
            .map_err(|error| failure(&error))?;
        if !(200..300).contains(&response.status) || denied_body(&response) {
            return check_response(response).map(|_| BTreeMap::new());
        }
        let token = public_token(&response.body, url).map_err(|error| failure(&error))?;
        Ok(BTreeMap::from([(String::from("anettokens"), token)]))
    }

    #[tracing::instrument(skip_all)]
    async fn one(
        &mut self,
        target: &Target,
        headers: &BTreeMap<String, String>,
    ) -> Result<Capture> {
        let url = policy::source_url(target, &target.url)?;
        if let Some(origin) = &target.browser_origin {
            let origin = policy::source_url(target, origin)?;
            ensure!(
                origin.host_str() == url.host_str(),
                "browser context and request host differ"
            );
        }
        if !self.uses_browser(target) {
            return self.fetch_http(target, headers).await;
        }
        crate::browser::ensure_safe_transport()?;
        policy::public_addresses(&url).await?;
        if self.browser.is_none() {
            self.browser = Some(
                Browser::start(
                    self.options.driver_url.as_deref(),
                    &self.options.profile,
                    self.options.headless,
                )
                .await?,
            );
        }
        self.pace(target).await?;
        self.browser
            .as_mut()
            .context("browser session was not initialized")?
            .fetch(target, headers)
            .await
    }

    const fn uses_browser(&self, target: &Target) -> bool {
        if matches!(target.kind, PageKind::Contacts) {
            return false;
        }
        match self.options.mode {
            Mode::Browser => true,
            Mode::Http => false,
            Mode::Auto => matches!(target.source, Source::AthleticNet),
        }
    }

    #[tracing::instrument(skip_all)]
    async fn pace(&mut self, target: &Target) -> Result<()> {
        let cap = if target.kind == PageKind::Contacts {
            if target.url.contains("staff-directory") {
                1.0 / 30.0
            } else {
                0.2
            }
        } else {
            1.0
        };
        let interval = Duration::try_from_secs_f64(self.options.rps.min(cap).recip())
            .context("request interval is not representable")?;
        let now = Instant::now();
        let start = self
            .next_request
            .get(&target.source)
            .copied()
            .map_or(now, |next| next.max(now));
        tokio::time::sleep_until(start).await;
        let next = Instant::now()
            .checked_add(interval)
            .context("request deadline overflow")?;
        self.next_request
            .entry(target.source)
            .and_modify(|deadline| *deadline = next)
            .or_insert(next);
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn close(self) -> Result<()> {
        match self.browser {
            Some(browser) => browser.close().await,
            None => Ok(()),
        }
    }
}

fn public_token(body: &[u8], url: &str) -> Result<String> {
    let value: Value = serde_json::from_slice(body).context("decoding public token preflight")?;
    let expected = if url.contains("GetCalendarCore") {
        "jwtTeamHomeCal"
    } else if url.contains("GetTeamCore") {
        "jwtTeamHome"
    } else {
        "jwtMeet"
    };
    let token = value
        .get(expected)
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .with_context(|| {
            format!("public token preflight omitted {expected}; access may require human review")
        })?;
    Ok(token.to_owned())
}

fn check_response(mut capture: Capture) -> Result<Capture, Failure> {
    let denied = denied_body(&capture);
    let blocked = access_stop_status(capture.status) || denied;
    capture.body = redact::body(&capture.body).map_err(|error| {
        let mut failure = failure(&error);
        if blocked {
            failure.detail = format!(
                "source returned HTTP {}; sanitizing response failed: {}",
                capture.status, failure.detail
            );
            failure.blocked = true;
        }
        failure
    })?;
    if !(200..300).contains(&capture.status) || denied {
        return Err(Failure {
            detail: format!(
                "source returned HTTP {}{}; no alternate transport attempted",
                capture.status,
                if denied {
                    " with an access challenge, login requirement, or paywall"
                } else {
                    ""
                }
            ),
            capture: Some(capture),
            blocked,
        });
    }
    Ok(capture)
}

fn denied_body(capture: &Capture) -> bool {
    let Ok(text) = std::str::from_utf8(&capture.body) else {
        return false;
    };
    if !capture.content_type.contains("html") {
        return false;
    }
    let text = text.to_ascii_lowercase();
    [
        "cf-chl-",
        "verify you are human",
        "checking your browser",
        "captcha challenge",
        "<title>access denied",
        "<title>just a moment",
        "sign in to continue",
        "subscribe to view this",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn failure(error: &anyhow::Error) -> Failure {
    let detail = format!("{error:#}");
    let lower = detail.to_ascii_lowercase();
    let blocked = error.is::<AccessStopCaptureError>()
        || [
            "challenge",
            "requires authentication",
            "paywall",
            "human review",
            "human handling",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
    Failure {
        capture: None,
        blocked,
        detail,
    }
}

pub fn redact_capture(mut capture: Capture) -> Result<Capture> {
    capture.body = redact::body(&capture.body)?;
    Ok(capture)
}

pub fn validate_target(target: &Target) -> Result<()> {
    policy::source_url(target, &target.url).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{Mode, Options, Transport};
    use crate::model::{PageContext, PageKind, Source, Target};

    #[tokio::test]
    async fn browser_modes_fail_before_driver_or_network_access() {
        for mode in [Mode::Auto, Mode::Browser] {
            let options = Options {
                mode,
                driver_url: Some("not a driver URL".to_owned()),
                profile: "/dev/null/invalid-profile".into(),
                headless: true,
                rps: 1.0,
            };
            let mut transport = Transport::new(options).expect("valid options");
            let mut target = Target::get(
                Source::AthleticNet,
                PageKind::Athlete,
                "https://www.athletic.net/athlete/1".to_owned(),
                PageContext::default(),
            );
            target.token_url = Some("https://www.athletic.net/token".to_owned());
            let error = transport
                .fetch(&target)
                .await
                .expect_err("browser must fail closed");
            assert!(error.detail.contains("browser transport disabled"));
            assert!(
                error
                    .detail
                    .contains("redirects, subresources, DNS resolution")
            );
            assert!(!error.blocked);
            assert!(transport.browser.is_none());
            assert!(transport.next_request.is_empty());
        }
    }
}
