mod protocol;

use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::Value;

use crate::model::{Capture, Target};
use protocol::Driver;

pub struct Browser {
    driver: Driver,
    session: String,
}

impl Browser {
    #[tracing::instrument(skip_all)]
    pub async fn start(driver_url: Option<&str>, profile: &Path, headless: bool) -> Result<Self> {
        ensure_safe_transport::<()>()?;
        tokio::fs::create_dir_all(profile)
            .await
            .context("creating dedicated browser profile directory")?;
        let profile = tokio::fs::canonicalize(profile)
            .await
            .context("resolving dedicated browser profile directory")?;
        let mut driver = Driver::start(driver_url).await?;
        match driver.create_session(&profile, headless).await {
            Ok(session) => Ok(Self { driver, session }),
            Err(error) => match driver.stop().await {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("driver cleanup also failed: {cleanup}")))
                }
            },
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn fetch(
        &self,
        _target: &Target,
        _headers: &BTreeMap<String, String>,
    ) -> Result<Capture> {
        ensure_safe_transport()
    }

    #[tracing::instrument(skip_all)]
    pub async fn close(mut self) -> Result<()> {
        let path = format!("session/{}", self.session);
        let session_result = self.driver.command(Method::DELETE, &path, None).await;
        let process_result = self.driver.stop().await;
        match (session_result, process_result) {
            (Ok(_), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("driver cleanup also failed: {cleanup}")))
            }
        }
    }
}

pub fn ensure_safe_transport<T>() -> Result<T> {
    bail!(
        "browser transport disabled: Chromium navigation redirects, subresources, DNS resolution, and page fetches cannot be validated and rate-limited before each network request; an enforceable browser egress boundary is required"
    )
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);

fn webdriver_error(value: &Value) -> Result<()> {
    let Some(code) = value.get("error").and_then(Value::as_str) else {
        return Ok(());
    };
    let description = match code {
        "session not created" => {
            "session not created (check Chromium, profile lock, and DISPLAY for headed mode)"
        }
        "invalid session id" => "invalid session id",
        "timeout" => "navigation timeout",
        "script timeout" => "script timeout",
        "javascript error" => "JavaScript execution failed",
        "invalid argument" => "invalid command argument",
        "unexpected alert open" => "unexpected browser prompt",
        "no such window" => "browser tab closed",
        "unknown error" => "driver/browser failure",
        _ => "unrecognized WebDriver failure",
    };
    bail!("WebDriver: {description}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Browser;

    #[tokio::test]
    async fn browser_start_fails_before_profile_or_driver_access() {
        let error = Browser::start(
            Some("not a driver URL"),
            Path::new("/dev/null/invalid-profile"),
            true,
        )
        .await
        .err()
        .expect("browser must fail closed");
        assert!(error.to_string().contains("browser transport disabled"));
    }
}
