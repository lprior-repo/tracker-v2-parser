mod audit;
mod capture;
mod exchange;
mod interception;
mod lifecycle;
mod native;
mod owned;
mod pacing;

#[cfg(test)]
mod review_tests;

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use crate::{
    model::{Capture, Target},
    transport::{
        AccessStopCaptureError, MAX_BODY_BYTES, access_stop_status, denied_capture, denied_url,
    },
};
use anyhow::{Context, Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Browser as Chromium, Page,
    browser::BrowserConfig,
    cdp::browser_protocol::{browser::BrowserContextId, target::CreateBrowserContextParams},
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tracing::Instrument;

use capture::{PageCapture, navigate};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);

fn handler_result(
    result: std::result::Result<(), chromiumoxide::error::CdpError>,
    shutdown_requested: bool,
) -> Result<()> {
    match result {
        Ok(()) if shutdown_requested => Ok(()),
        Ok(()) => Err(anyhow!(
            "Chromium event handler ended without owned shutdown"
        )),
        Err(error)
            if shutdown_requested && matches!(&error, chromiumoxide::error::CdpError::Ws(_)) =>
        {
            tracing::debug!(%error, "owned Chromium transport closed during requested shutdown");
            Ok(())
        }
        Err(error) => Err(anyhow!(error).context("Chromium event handler failed")),
    }
}

pub struct Browser {
    chromium: Arc<owned::OwnedChromium>,
    context: Option<BrowserContextId>,
    native_sink: native::NativeSink,
    fetches: Mutex<JoinSet<Result<()>>>,
    handler: JoinHandle<Result<()>>,
    #[cfg(test)]
    capture_ready: Option<Arc<tokio::sync::Notify>>,
    #[cfg(test)]
    local_origin: Option<url::Origin>,
    #[cfg(test)]
    page_deadline: Option<Duration>,
    #[cfg(test)]
    additional_origin: Option<url::Origin>,
    #[cfg(test)]
    fail_page_close: bool,
}

impl Browser {
    #[tracing::instrument(skip_all)]
    pub async fn start(driver_url: Option<&str>, profile: &Path, headless: bool) -> Result<Self> {
        ensure!(
            driver_url.is_none(),
            "external CDP driver URLs are unsupported: protected acquisition requires locally owned Chromium"
        );
        let native_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding browser native-transport sink")?;
        let proxy = format!("http://{}", native_listener.local_addr()?);
        tokio::fs::create_dir_all(profile)
            .await
            .context("creating browser profile")?;
        let mut config = BrowserConfig::builder()
            .user_data_dir(profile)
            .respect_https_errors()
            .request_timeout(COMMAND_TIMEOUT)
            .launch_timeout(COMMAND_TIMEOUT)
            .arg("--disable-quic")
            .arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
        if !headless {
            config = config.with_head();
        }
        let config = config
            .build()
            .map_err(|error| anyhow!("building browser configuration: {error}"))?;
        let native_sink = native::NativeSink::start(native_listener)?;
        let launched = timeout(COMMAND_TIMEOUT, Chromium::launch(config))
            .await
            .context("launching Chromium timed out")
            .and_then(|result| result.map_err(Into::into));
        let (browser, handler) =
            match launched {
                Ok(value) => value,
                Err(error) => {
                    return match native_sink.finish().await {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(error
                            .context(format!("native sink startup cleanup failed: {cleanup:#}"))),
                    };
                }
            };
        let browser = Arc::new(owned::OwnedChromium::new(browser));
        let shutdown = browser.shutdown_signal();
        let handler = tokio::spawn(async move {
            let result = handler
                .try_for_each(|()| {
                    futures::future::ready(Ok::<(), chromiumoxide::error::CdpError>(()))
                })
                .await;
            handler_result(result, shutdown.load(std::sync::atomic::Ordering::Acquire))
        });
        let mut browser = Self {
            chromium: browser,
            context: None,
            native_sink,
            fetches: Mutex::new(JoinSet::new()),
            handler,
            #[cfg(test)]
            local_origin: None,
            #[cfg(test)]
            page_deadline: None,
            #[cfg(test)]
            additional_origin: None,
            #[cfg(test)]
            fail_page_close: false,
            #[cfg(test)]
            capture_ready: None,
        };
        let params = CreateBrowserContextParams::builder()
            .dispose_on_detach(true)
            .proxy_server(proxy)
            .proxy_bypass_list("<-loopback>")
            .build();
        let context = timeout(
            COMMAND_TIMEOUT,
            browser.chromium.create_browser_context(params),
        )
        .await
        .context("creating failclosed browser context timed out")
        .and_then(std::convert::identity);
        match context {
            Ok(context) => {
                browser.context = Some(context);
                Ok(browser)
            }
            Err(error) => match browser.close().await {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("browser startup cleanup failed: {cleanup:#}")))
                }
            },
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn fetch(
        &self,
        target: &Target,
        headers: &BTreeMap<String, String>,
        interval: Duration,
    ) -> Result<Capture> {
        let deadline = interval
            .checked_add(COMMAND_TIMEOUT.saturating_mul(2))
            .context("browser deadline overflow")?;
        #[cfg(test)]
        let deadline = self.page_deadline.map_or(deadline, |value| value);
        let acquisition = lifecycle::Acquisition {
            chromium: Arc::clone(&self.chromium),
            context: self
                .context
                .clone()
                .context("browser acquisition context is unavailable")?,
            target: target.clone(),
            headers: headers.clone(),
            interval,
            deadline,
            native_status: self.native_sink.status(),
            #[cfg(test)]
            local_origin: self.local_origin.clone(),
            #[cfg(test)]
            additional_origin: self.additional_origin.clone(),
            #[cfg(test)]
            fail_page_close: self.fail_page_close,
            #[cfg(test)]
            capture_ready: self.capture_ready.clone(),
        };
        let (reply, received) = oneshot::channel();
        {
            let mut tasks = self.fetches.lock().await;
            if let Some(previous) = tasks.try_join_next() {
                previous.context("joining prior browser acquisition")??;
            }
            ensure!(tasks.is_empty(), "browser acquisition capacity exceeded");
            tasks.spawn(
                acquisition
                    .run(reply)
                    .instrument(tracing::info_span!("owned_browser_acquisition")),
            );
            drop(tasks);
        }
        let result = received
            .await
            .context("browser acquisition supervisor stopped")?;
        let cleanup = lifecycle::drain(&mut *self.fetches.lock().await).await;
        finish_page_fetch(result, cleanup)
    }

    async fn fetch_page(
        page: &Page,
        target: &Target,
        headers: &BTreeMap<String, String>,
        interval: Duration,
    ) -> Result<PageCapture> {
        let method = target.method.as_deref().map_or("GET", |value| value);
        ensure!(
            matches!(method, "GET" | "POST"),
            "source only supports GET and POST requests"
        );
        if let Some(origin) = &target.browser_origin {
            let bootstrap = navigate(page, origin).await?;
            if access_stop_status(bootstrap.status)
                || denied_capture(&bootstrap)
                || !(200..300).contains(&bootstrap.status)
            {
                return Ok(PageCapture::Response(bootstrap));
            }
            tokio::time::sleep(interval).await;
            return browser_fetch(page, target, headers, method)
                .await
                .map(PageCapture::Response);
        }
        if method == "GET" && target.body.is_none() && headers.is_empty() {
            return capture::document(page, &target.url)
                .await
                .map(PageCapture::Navigation);
        }
        let url = url::Url::parse(&target.url).context("parsing browser target")?;
        let origin = url.origin().ascii_serialization();
        let bootstrap = navigate(page, &origin).await?;
        if access_stop_status(bootstrap.status)
            || denied_capture(&bootstrap)
            || !(200..300).contains(&bootstrap.status)
        {
            return Ok(PageCapture::Response(bootstrap));
        }
        tokio::time::sleep(interval).await;
        browser_fetch(page, target, headers, method)
            .await
            .map(PageCapture::Response)
    }

    #[tracing::instrument(skip_all)]
    pub async fn close(mut self) -> Result<()> {
        let acquired = lifecycle::drain(self.fetches.get_mut()).await;
        let retained = self.chromium.release_retained().await;
        let disposed = match self.context.take() {
            Some(context) => timeout(
                COMMAND_TIMEOUT,
                self.chromium.dispose_browser_context(context),
            )
            .await
            .context("disposing browser context timed out")
            .and_then(std::convert::identity),
            None => Ok(()),
        };
        self.chromium.request_shutdown();
        let (mut chromium, retained_guard) = Arc::try_unwrap(self.chromium)
            .map_err(|_| anyhow!("browser acquisition retained Chromium after drain"))?
            .into_inner();
        let close = timeout(COMMAND_TIMEOUT, chromium.close())
            .await
            .context("closing Chromium timed out")
            .and_then(|result| result.map(|_| ()).map_err(Into::into));
        let waited = if close.is_ok() {
            timeout(COMMAND_TIMEOUT, chromium.wait())
                .await
                .context("waiting for Chromium timed out")
                .and_then(|result| result.map(|_| ()).map_err(Into::into))
        } else {
            Ok(())
        };
        let killed = if close.is_err() || waited.is_err() {
            chromium
                .kill()
                .await
                .map_or_else(|| Ok(()), |result| result.context("killing Chromium"))
        } else {
            Ok(())
        };
        if close.is_err() || waited.is_err() {
            self.handler.abort();
        }
        let joined = lifecycle::join_handler(self.handler).await;
        let retained_guard = match retained_guard {
            Some(interceptor) => interceptor.finish(false).await,
            None => Ok(()),
        };
        let native = self.native_sink.finish().await;
        acquired
            .and(disposed)
            .and(retained)
            .and(retained_guard)
            .and(close)
            .and(waited)
            .and(killed)
            .and(joined)
            .and(native)
    }
}

fn finish_page_fetch(result: Result<Capture>, cleanup: Result<()>) -> Result<Capture> {
    match (result, cleanup) {
        (Ok(capture), Ok(())) => Ok(capture),
        (Ok(capture), Err(error))
            if access_stop_status(capture.status) || denied_capture(&capture) =>
        {
            tracing::warn!(%error, "browser page cleanup failed after access stop");
            Ok(capture)
        }
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("page cleanup also failed: {cleanup:#}")))
        }
    }
}

#[derive(Serialize)]
struct FetchArgs<'a> {
    url: &'a str,
    method: &'a str,
    headers: &'a BTreeMap<String, String>,
    body: Option<String>,
    max_body: usize,
}

#[derive(Deserialize)]
struct FetchResult {
    url: String,
    status: u16,
    content_type: String,
    body: String,
    error: Option<String>,
}

async fn browser_fetch(
    page: &Page,
    target: &Target,
    headers: &BTreeMap<String, String>,
    method: &str,
) -> Result<Capture> {
    let args = FetchArgs {
        url: &target.url,
        method,
        headers,
        body: target
            .body
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        max_body: MAX_BODY_BYTES,
    };
    let expression = format!("({FETCH_SCRIPT})({})", serde_json::to_string(&args)?);
    let value: FetchResult = page
        .evaluate_expression(
            chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
                .expression(expression)
                .await_promise(true)
                .return_by_value(true)
                .build()
                .map_err(|error| anyhow!("building browser fetch: {error}"))?,
        )
        .await
        .context("executing browser fetch")?
        .into_value()
        .context("decoding browser fetch result")?;
    let body = match value.error {
        Some(detail) => Err(anyhow!("{detail}")),
        None => decode_body(&value.body),
    }
    .map_err(|error| {
        if access_stop_status(value.status) || denied_url(&value.url) {
            AccessStopCaptureError {
                status: value.status,
                error,
            }
            .into()
        } else {
            error
        }
    })?;
    Ok(Capture {
        url: value.url,
        status: value.status,
        content_type: value.content_type,
        body,
    })
}

fn decode_body(value: &str) -> Result<Vec<u8>> {
    let max_encoded = MAX_BODY_BYTES
        .saturating_add(2)
        .saturating_div(3)
        .saturating_mul(4);
    ensure!(
        value.len() <= max_encoded,
        "source response exceeded 16 MiB limit"
    );
    let body = STANDARD
        .decode(value)
        .context("decoding browser response")?;
    ensure!(
        body.len() <= MAX_BODY_BYTES,
        "source response exceeded 16 MiB limit"
    );
    Ok(body)
}

const FETCH_SCRIPT: &str = r"async function(spec) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 40000);
    try {
        const headers = new Headers(spec.headers);
        if (spec.body !== null && !headers.has('content-type')) headers.set('content-type', 'application/json');
        const response = await fetch(spec.url, {method: spec.method, headers, body: spec.body, credentials: 'include', signal: controller.signal});
        const reader = response.body && response.body.getReader();
        const chunks = [];
        let size = 0;
        try {
            if (reader) {
                for (;;) {
                    const part = await reader.read();
                    if (part.done) break;
                    size += part.value.byteLength;
                    if (size > spec.max_body) { controller.abort(); throw new Error('source response exceeded 16 MiB limit'); }
                    chunks.push(part.value);
                }
            }
        } catch (error) {
            return {url: response.url, status: response.status, content_type: response.headers.get('content-type') || '', body: '', error: String(error)};
        }
        let binary = '';
        for (const chunk of chunks) {
            for (let offset = 0; offset < chunk.length; offset += 8192) binary += String.fromCharCode(...chunk.subarray(offset, offset + 8192));
        }
        return {url: response.url, status: response.status, content_type: response.headers.get('content-type') || '', body: btoa(binary)};
    } finally { clearTimeout(timer); }
}";

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{Browser, finish_page_fetch};
    use crate::{
        model::{PageContext, PageKind, Source, Target},
        transport::check_response,
    };

    #[tokio::test]
    async fn chromium_captures_navigation_requests_and_stops_on_challenge() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let api_requests = Arc::new(AtomicUsize::new(0));
        let observed_requests = Arc::clone(&api_requests);
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let api_requests = Arc::clone(&observed_requests);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        let count = match socket.read(&mut buffer).await {
                            Ok(value) => value,
                            Err(_) => return,
                        };
                        if count == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&bytes[..end]);
                            let length = header
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|value| value.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    if request.starts_with("GET /api ") || request.starts_with("POST /api ") {
                        api_requests.fetch_add(1, Ordering::SeqCst);
                    }
                    let redirect = if request.starts_with("GET /redirect-login ") {
                        Some("/login")
                    } else if request.starts_with("GET /redirect-subscribe ") {
                        Some("/subscribe")
                    } else if request.starts_with("GET /api-redirect ") {
                        Some("/signin")
                    } else {
                        None
                    };
                    if let Some(location) = redirect {
                        let response = format!(
                            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        if let Err(error) = socket.write_all(response.as_bytes()).await {
                            eprintln!("fixture write failed: {error}");
                        }
                        return;
                    }
                    let (status, content_type, body) = if request.starts_with("GET /challenge ") {
                        (
                            "403 Forbidden",
                            "text/html",
                            "<title>Just a moment</title>".to_owned(),
                        )
                    } else if request.starts_with("GET /render ") {
                        ("201 Created", "text/html", "<html><body><script>document.body.insertAdjacentHTML('beforeend','<p>rendered</p>')</script></body></html>".to_owned())
                    } else if request.starts_with("GET /api ")
                        && request.contains("x-test-token: value")
                    {
                        (
                            "202 Accepted",
                            "application/json",
                            r#"{"season":2026}"#.to_owned(),
                        )
                    } else if request.starts_with("POST /api ")
                        && request.contains("x-test-token: value")
                        && request.contains("application/json")
                        && request.contains(r#"{"season":2026}"#)
                    {
                        (
                            "202 Accepted",
                            "application/json",
                            r#"{"saved":true}"#.to_owned(),
                        )
                    } else if request.starts_with("GET /api ") || request.starts_with("POST /api ")
                    {
                        ("400 Bad Request", "application/json", "{}".to_owned())
                    } else {
                        (
                            "200 OK",
                            "text/html",
                            "<html><body>origin</body></html>".to_owned(),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    if let Err(error) = socket.write_all(response.as_bytes()).await {
                        eprintln!("fixture write failed: {error}");
                    }
                });
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = Browser::start(None, profile.path(), true).await?;
        browser.local_origin = Some(url::Url::parse(&origin)?.origin());
        let scenario = async {
            let mut target = Target::get(
                Source::AthleticNet,
                PageKind::Athlete,
                format!("{origin}/render"),
                PageContext::default(),
            );
            let rendered = browser
                .fetch(&target, &BTreeMap::new(), Duration::ZERO)
                .await?;
            assert_eq!(rendered.status, 201);
            assert!(String::from_utf8(rendered.body)?.contains("<p>rendered</p>"));
            target.url = format!("{origin}/api");
            target.browser_origin = Some(origin.clone());
            let headers = BTreeMap::from([("x-test-token".to_owned(), "value".to_owned())]);
            let get = browser.fetch(&target, &headers, Duration::ZERO).await?;
            assert_eq!(get.status, 202);
            assert!(get.content_type.contains("application/json"));
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&get.body)?,
                serde_json::json!({"season": 2026})
            );
            target.method = Some("POST".to_owned());
            target.body = Some(serde_json::json!({"season": 2026}));
            let post = browser.fetch(&target, &headers, Duration::ZERO).await?;
            assert_eq!(post.status, 202);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&post.body)?,
                serde_json::json!({"saved": true})
            );
            target.browser_origin = Some(format!("{origin}/challenge"));
            let challenge = browser.fetch(&target, &headers, Duration::ZERO).await?;
            assert_eq!(challenge.status, 403);
            assert!(String::from_utf8_lossy(&challenge.body).contains("Just a moment"));
            let preserved = finish_page_fetch(
                Ok(challenge),
                Err(anyhow::anyhow!("page closed unexpectedly")),
            )?;
            let failure = check_response(preserved).expect_err("challenge must remain blocked");
            assert!(failure.blocked);
            let original = anyhow::Error::new(crate::transport::AccessStopCaptureError {
                status: 403,
                error: anyhow::anyhow!("capture failed"),
            });
            let combined =
                finish_page_fetch(Err(original), Err(anyhow::anyhow!("page close failed")))
                    .expect_err("capture failure must survive cleanup failure");
            assert!(combined.is::<crate::transport::AccessStopCaptureError>());
            assert!(format!("{combined:#}").contains("page close failed"));
            assert_eq!(failure.capture.expect("challenge capture").status, 403);
            let before = api_requests.load(Ordering::SeqCst);
            target.browser_origin = Some(format!("{origin}/redirect-subscribe"));
            let redirected = browser
                .fetch(&target, &headers, Duration::ZERO)
                .await
                .expect_err("subscription must stop before capture");
            let failure = crate::transport::failure(&redirected);
            assert_eq!(failure.stopped_origin.as_deref(), Some(origin.as_str()));
            assert!(failure.blocked);
            assert_eq!(api_requests.load(Ordering::SeqCst), before);
            target.method = None;
            target.body = None;
            target.browser_origin = None;
            target.url = format!("{origin}/redirect-login");
            let redirected = browser
                .fetch(&target, &BTreeMap::new(), Duration::ZERO)
                .await
                .expect_err("login must stop before capture");
            let failure = crate::transport::failure(&redirected);
            assert!(failure.blocked);
            assert_eq!(failure.stopped_origin.as_deref(), Some(origin.as_str()));
            target.browser_origin = Some(origin.clone());
            target.url = format!("{origin}/api-redirect");
            let redirected = browser
                .fetch(&target, &headers, Duration::ZERO)
                .await
                .expect_err("signin must stop before capture");
            let failure = crate::transport::failure(&redirected);
            assert!(failure.blocked);
            assert_eq!(failure.stopped_origin.as_deref(), Some(origin.as_str()));
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let closed = browser.close().await;
        server.abort();
        scenario.and(closed)
    }
}
