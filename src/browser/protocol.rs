use std::{net::IpAddr, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use url::{Host, Url};

use super::{COMMAND_TIMEOUT, webdriver_error};

const MAX_REPLY_BYTES: usize = 24 * 1024 * 1024;
const MAX_REPLY_CHUNKS: usize = 65_536;

pub(super) struct Driver {
    client: Client,
    endpoint: Url,
    child: Option<Child>,
}

impl Driver {
    #[tracing::instrument(skip_all)]
    pub(super) async fn start(address: Option<&str>) -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(COMMAND_TIMEOUT)
            .build()
            .context("building local WebDriver client")?;
        let (endpoint, child) = match address {
            Some(address) => (parse_endpoint(address)?, None),
            None => spawn_driver().await?,
        };
        let mut driver = Self {
            client,
            endpoint,
            child,
        };
        if let Err(error) = driver.wait_ready().await {
            return match driver.stop().await {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("driver cleanup also failed: {cleanup}")))
                }
            };
        }
        Ok(driver)
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn create_session(&self, profile: &Path, headless: bool) -> Result<String> {
        let profile = profile
            .to_str()
            .context("browser profile path must be valid UTF-8")?;
        let mut arguments = vec![format!("--user-data-dir={profile}")];
        if headless {
            arguments.push("--headless=new".to_owned());
        }
        let value = self
            .command(
                Method::POST,
                "session",
                Some(json!({"capabilities": {"alwaysMatch": {
                    "browserName": "chrome",
                    "pageLoadStrategy": "normal",
                    "acceptInsecureCerts": false,
                    "timeouts": {"script": 35_000, "pageLoad": 30_000, "implicit": 0},
                    "goog:chromeOptions": {"args": arguments}
                }}})),
            )
            .await?;
        let session = value
            .get("sessionId")
            .and_then(Value::as_str)
            .context("WebDriver did not return a session id")?;
        ensure!(
            !session.is_empty()
                && session.len() <= 256
                && session
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
            "WebDriver returned an invalid session id"
        );
        Ok(session.to_owned())
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn command(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let address = self.endpoint.join(path).context("forming driver command")?;
        let request = self.client.request(method, address);
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        let response = request
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .context("sending local WebDriver command")?;
        let status = response.status();
        let bytes = read_reply(response).await?;
        let mut reply: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("WebDriver returned malformed JSON (HTTP {status})"))?;
        let value = reply
            .get("value")
            .context("WebDriver reply missing value")?;
        webdriver_error(value)?;
        ensure!(
            status.is_success(),
            "WebDriver command failed with HTTP {status}"
        );
        reply
            .get_mut("value")
            .map(Value::take)
            .context("WebDriver reply missing value")
    }

    #[tracing::instrument(skip_all)]
    async fn wait_ready(&self) -> Result<()> {
        let address = self
            .endpoint
            .join("status")
            .context("forming driver status URL")?;
        let address = &address;
        let ready = stream::iter(0..40)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(false, |ready, _| async move {
                if ready || self.probe_ready(address).await {
                    return Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(false)
            })
            .await?;
        ensure!(
            ready,
            "WebDriver did not become ready within 40 bounded attempts"
        );
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn probe_ready(&self, address: &Url) -> bool {
        let response = match self
            .client
            .get(address.clone())
            .timeout(Duration::from_millis(500))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            Ok(_) | Err(_) => return false,
        };
        let Ok(bytes) = read_reply(response).await else {
            return false;
        };
        let value = serde_json::from_slice::<Value>(&bytes);
        matches!(&value, Ok(value) if value.pointer("/value/ready").and_then(Value::as_bool) == Some(true))
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn stop(&mut self) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if child
            .try_wait()
            .context("checking owned chromedriver exit")?
            .is_some()
        {
            self.child = None;
            return Ok(());
        }
        child
            .start_kill()
            .context("terminating owned chromedriver")?;
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .context("timed out reaping owned chromedriver")?
            .context("reaping owned chromedriver")?;
        self.child = None;
        Ok(())
    }
}

fn parse_endpoint(address: &str) -> Result<Url> {
    let mut url = Url::parse(address).context("parsing external WebDriver URL")?;
    ensure!(
        url.scheme() == "http",
        "WebDriver URL must use loopback HTTP"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "WebDriver URL must not contain credentials, a query, or a fragment"
    );
    match url.host() {
        Some(Host::Ipv4(host)) if IpAddr::V4(host).is_loopback() => {}
        Some(Host::Ipv6(host)) if IpAddr::V6(host).is_loopback() => {}
        Some(Host::Domain("localhost")) => {
            url.set_host(Some("127.0.0.1"))
                .map_err(|_| anyhow!("setting loopback driver host"))?;
        }
        _ => bail!("WebDriver URL must name a loopback host"),
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[tracing::instrument(skip_all)]
async fn spawn_driver() -> Result<(Url, Option<Child>)> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .context("selecting loopback chromedriver port")?;
    let port = listener
        .local_addr()
        .context("reading chromedriver port")?
        .port();
    let endpoint = parse_endpoint(&format!("http://127.0.0.1:{port}/"))?;
    drop(listener);
    let child = Command::new("chromedriver")
        .arg(format!("--port={port}"))
        .arg("--silent")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting chromedriver from PATH")?;
    Ok((endpoint, Some(child)))
}

#[tracing::instrument(skip_all)]
async fn read_reply(response: reqwest::Response) -> Result<Vec<u8>> {
    response
        .bytes_stream()
        .take(MAX_REPLY_CHUNKS.saturating_add(1))
        .map_err(|error| anyhow::Error::new(error.without_url()))
        .try_fold(
            (Vec::new(), 0_usize),
            |(mut bytes, count), chunk| async move {
                ensure!(
                    count < MAX_REPLY_CHUNKS,
                    "WebDriver response exceeded chunk limit"
                );
                let length = bytes
                    .len()
                    .checked_add(chunk.len())
                    .context("WebDriver response size overflow")?;
                ensure!(
                    length <= MAX_REPLY_BYTES,
                    "WebDriver response exceeded 24 MiB limit"
                );
                bytes.extend_from_slice(&chunk);
                Ok((bytes, count.saturating_add(1)))
            },
        )
        .await
        .map(|(bytes, _)| bytes)
        .context("reading bounded WebDriver response")
}

#[cfg(test)]
mod tests {
    use super::parse_endpoint;

    #[test]
    fn given_driver_address_when_parsed_then_only_loopback_http_is_accepted() {
        for address in [
            "http://127.0.0.1:9515",
            "http://[::1]:9515/wd/hub",
            "http://localhost:9515/",
        ] {
            assert!(parse_endpoint(address).is_ok(), "{address}");
        }
        for address in [
            "https://127.0.0.1:9515",
            "http://example.com:9515",
            "http://127.0.0.1.example.com",
            "http://user:secret@localhost",
            "http://localhost/?secret=yes",
        ] {
            assert!(parse_endpoint(address).is_err(), "{address}");
        }
        assert_eq!(
            parse_endpoint("http://localhost:9515/wd/hub")
                .expect("loopback endpoint")
                .as_str(),
            "http://127.0.0.1:9515/wd/hub/"
        );
    }
}
