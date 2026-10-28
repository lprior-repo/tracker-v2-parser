mod http;
pub(crate) mod policy;
mod redact;
pub(crate) mod robots;

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
    pub stopped_origin: Option<Box<str>>,
    pub capture: Option<Capture>,
    pub blocked: bool,
    pub detail: Box<str>,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for Failure {}

#[derive(Debug, Clone)]
pub(crate) struct AccessStop {
    pub origin: String,
    pub status: Option<u16>,
}

impl fmt::Display for AccessStop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source access stopped at {} (HTTP {:?}); human review required",
            self.origin, self.status
        )
    }
}

impl std::error::Error for AccessStop {}

#[derive(Debug)]
pub(crate) struct AccessStopCaptureError {
    pub(crate) origin: Option<String>,
    pub(crate) status: u16,
    pub(crate) error: anyhow::Error,
}

impl AccessStopCaptureError {
    pub(crate) fn new(status: u16, error: anyhow::Error, address: &str) -> Self {
        let origin = url::Url::parse(address)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .map(|url| url.origin().ascii_serialization());
        Self {
            origin,
            status,
            error,
        }
    }
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

pub(crate) const fn access_stop_status(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 429 | 451)
}

pub struct Transport {
    options: Options,
    browser: Option<Browser>,
    clients: BTreeMap<String, Client>,
    next_request: BTreeMap<Source, Instant>,
    #[cfg(test)]
    stall_dns: bool,
    #[cfg(test)]
    http_test_budget: Option<Duration>,
    #[cfg(test)]
    http_test_url: Option<url::Url>,
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
            #[cfg(test)]
            stall_dns: false,
            #[cfg(test)]
            http_test_budget: None,
            #[cfg(test)]
            http_test_url: None,
        })
    }

    #[tracing::instrument(skip_all)]
    pub async fn fetch(&mut self, target: &Target) -> Result<Capture, Failure> {
        robots::check(&target.url, &target.url).map_err(|error| failure(&error))?;
        let headers = self.token_headers(target).await?;
        let response = self
            .one(target, &headers)
            .await
            .map_err(|error| failure(&error))?;
        prepare_response(target.kind, response)
    }

    #[tracing::instrument(skip_all)]
    async fn token_headers(
        &mut self,
        target: &Target,
    ) -> Result<BTreeMap<String, String>, Failure> {
        let Some(url) = &target.token_url else {
            return Ok(BTreeMap::new());
        };
        robots::check(url, &target.url).map_err(|error| failure(&error))?;
        let mut preflight = target.clone();
        preflight.url.clone_from(url);
        preflight.token_url = None;
        preflight.method = None;
        preflight.body = None;
        let response = self
            .one(&preflight, &BTreeMap::new())
            .await
            .map_err(|error| failure(&error))?;
        if !(200..300).contains(&response.status) || denied_capture(&response) {
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
        robots::check(&target.url, &target.url)?;
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
        let interval = request_interval(target, self.options.rps)?;
        let result = self
            .browser
            .as_ref()
            .context("browser session was not initialized")?
            .fetch(target, headers, interval)
            .await;
        self.pace(target).await?;
        result
    }

    const fn uses_browser(&self, target: &Target) -> bool {
        if matches!(target.kind, PageKind::Contacts) {
            return true;
        }
        match self.options.mode {
            Mode::Browser => true,
            Mode::Http => false,
            Mode::Auto => matches!(target.source, Source::AthleticNet),
        }
    }

    #[tracing::instrument(skip_all)]
    async fn pace(&mut self, target: &Target) -> Result<()> {
        let interval = request_interval(target, self.options.rps)?;
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

fn request_interval(target: &Target, rps: f64) -> Result<Duration> {
    let cap = if target.kind == PageKind::Contacts {
        if target.url.contains("staff-directory") {
            1.0 / 30.0
        } else {
            0.2
        }
    } else {
        1.0
    };
    let interval = Duration::try_from_secs_f64(rps.min(cap).recip())
        .context("request interval is not representable")?;
    Ok(interval)
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

pub(crate) fn prepare_response(kind: PageKind, mut capture: Capture) -> Result<Capture, Failure> {
    if kind != PageKind::Contacts {
        return check_response(capture);
    }
    let denied = denied_url(&capture.url);
    if !(200..300).contains(&capture.status) || denied {
        capture.body.clear();
    }
    finish_status(capture, denied)
}

pub(crate) fn check_response(capture: Capture) -> Result<Capture, Failure> {
    let denied = denied_capture(&capture);
    finish_response(capture, denied)
}

fn finish_response(mut capture: Capture, denied: bool) -> Result<Capture, Failure> {
    let blocked = access_stop_status(capture.status) || denied;
    capture.body = redact::body(&capture.body).map_err(|error| {
        let mut failure = failure(&error);
        if blocked {
            failure.detail = format!(
                "source returned HTTP {}; sanitizing response failed: {}",
                capture.status, failure.detail
            )
            .into_boxed_str();
            failure.blocked = true;
        }
        failure
    })?;
    finish_status(capture, denied)
}

fn finish_status(capture: Capture, denied: bool) -> Result<Capture, Failure> {
    let blocked = access_stop_status(capture.status) || denied;
    if !(200..300).contains(&capture.status) || denied {
        return Err(Failure {
            stopped_origin: None,
            detail: format!(
                "source returned HTTP {}{}; no alternate transport attempted",
                capture.status,
                if denied {
                    " with an access challenge, login requirement, or paywall"
                } else {
                    ""
                }
            )
            .into_boxed_str(),
            capture: Some(capture),
            blocked,
        });
    }
    Ok(capture)
}

pub(crate) fn denied_capture(capture: &Capture) -> bool {
    denied_url(&capture.url) || denied_body(capture)
}

pub(crate) fn denied_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.path_segments().is_some_and(|parts| {
            parts.take(4).any(|part| {
                ["login", "signin", "subscribe"]
                    .iter()
                    .any(|name| part.eq_ignore_ascii_case(name))
            })
        })
    })
}

pub(crate) fn denied_body(capture: &Capture) -> bool {
    let Ok(text) = std::str::from_utf8(&capture.body) else {
        return false;
    };
    let trimmed = text.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    if !capture.content_type.to_ascii_lowercase().contains("html") && !trimmed.starts_with('<') {
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

pub(crate) fn failure(error: &anyhow::Error) -> Failure {
    let detail = format!("{error:#}");
    let lower = detail.to_ascii_lowercase();
    let stopped = error.downcast_ref::<AccessStop>();
    let blocked = stopped.is_some()
        || error.is::<AccessStopCaptureError>()
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
        stopped_origin: stopped
            .map(|stop| stop.origin.as_str())
            .or_else(|| {
                error
                    .downcast_ref::<AccessStopCaptureError>()
                    .and_then(|stop| stop.origin.as_deref())
            })
            .map(|origin| origin.to_owned().into_boxed_str()),
        capture: None,
        blocked,
        detail: detail.into_boxed_str(),
    }
}

pub fn redact_capture(mut capture: Capture) -> Result<Capture> {
    capture.body = redact::body(&capture.body)?;
    Ok(capture)
}

pub(crate) fn redact_public_json(value: &Value) -> Result<String> {
    let encoded = serde_json::to_vec(value).context("encoding public evidence")?;
    String::from_utf8(redact::body(&encoded)?).context("decoding sanitized public evidence")
}

pub fn validate_target(target: &Target) -> Result<()> {
    let url = policy::source_url(target, &target.url)?;
    if let Some(address) = &target.browser_origin {
        let origin = policy::source_url(target, address)?;
        ensure!(
            origin.origin() == url.origin(),
            "browser context and request origin differ"
        );
    }
    if let Some(address) = &target.token_url {
        let token = policy::source_url(target, address)?;
        ensure!(
            token.origin() == url.origin(),
            "token preflight and request origin differ"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::model::PageKind;

    fn contact_capture(url: &str, status: u16) -> crate::model::Capture {
        crate::model::Capture {
            url: url.into(),
            status,
            content_type: "text/html".into(),
            body: b"<form><input type='password' value='transient-canary'></form><script>const userToken = getToken('transient-canary');</script>".to_vec(),
        }
    }

    #[test]
    fn given_transient_contact_markup_when_prepared_then_body_survives_but_provider_redaction_stays_strict()
     {
        let capture = super::prepare_response(
            PageKind::Contacts,
            contact_capture("https://school.example.edu/staff", 200),
        )
        .expect("public transient contact page");
        assert!(String::from_utf8_lossy(&capture.body).contains("transient-canary"));
        let failure = super::prepare_response(PageKind::Athlete, capture)
            .expect_err("provider captures remain strict");
        assert!(failure.capture.is_none());
        assert!(!format!("{failure:?}").contains("transient-canary"));
    }

    #[test]
    fn given_contact_http_failure_or_denied_url_when_prepared_then_status_survives_without_raw_body()
     {
        for (url, status, blocked) in [
            ("https://school.example.edu/staff", 403, true),
            ("https://school.example.edu/staff", 500, false),
            ("https://school.example.edu/login", 200, true),
        ] {
            let failure = super::prepare_response(PageKind::Contacts, contact_capture(url, status))
                .expect_err("nonpublic response");
            assert_eq!(failure.blocked, blocked);
            assert!(!format!("{failure:?}").contains("transient-canary"));
            assert!(!failure.to_string().contains("transient-canary"));
            let capture = failure.capture.expect("status metadata");
            assert_eq!(capture.status, status);
            assert_eq!(capture.url, url);
            assert!(capture.body.is_empty());
        }
    }

    #[test]
    fn access_stop_urls_require_complete_path_segments() {
        assert!(super::denied_url(
            "https://example.com/account/LOGIN?next=/"
        ));
        assert!(super::denied_url("https://example.com/subscribe"));
        assert!(!super::denied_url("https://example.com/login-results"));
        assert!(!super::denied_url(
            "https://example.com/results?next=/login"
        ));
    }
}

#[cfg(test)]
mod pacing_tests {
    use super::*;
    #[test]
    fn given_contact_targets_when_interval_is_selected_then_wire_uses_contact_caps() -> Result<()> {
        let mut target = Target::get(
            Source::MileSplit,
            PageKind::Contacts,
            "https://example.edu/staff-directory".into(),
            Default::default(),
        );
        assert_eq!(request_interval(&target, 8.0)?, Duration::from_secs(30));
        target.url = "https://example.edu/contact".into();
        assert_eq!(request_interval(&target, 8.0)?, Duration::from_secs(5));
        target.kind = PageKind::Athlete;
        assert_eq!(request_interval(&target, 8.0)?, Duration::from_secs(1));
        Ok(())
    }
}
