use std::{collections::BTreeMap, path::Path, time::Duration};

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
    cdp::browser_protocol::network::{
        self, EventLoadingFinished, EventResponseReceived, GetResponseBodyParams, ResourceType,
    },
    handler::HandlerConfig,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{task::JoinHandle, time::timeout};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);

pub struct Browser {
    chromium: Chromium,
    handler: JoinHandle<Result<()>>,
    owned: bool,
}

impl Browser {
    #[tracing::instrument(skip_all)]
    pub async fn start(driver_url: Option<&str>, profile: &Path, headless: bool) -> Result<Self> {
        let (browser, handler, owned) = if let Some(endpoint) = driver_url {
            let config = HandlerConfig {
                ignore_https_errors: false,
                request_timeout: COMMAND_TIMEOUT,
                ..HandlerConfig::default()
            };
            let (browser, handler) = timeout(
                COMMAND_TIMEOUT,
                Chromium::connect_with_config(endpoint, config),
            )
            .await
            .context("connecting to Chromium timed out")??;
            (browser, handler, false)
        } else {
            tokio::fs::create_dir_all(profile)
                .await
                .context("creating browser profile")?;
            let mut config = BrowserConfig::builder()
                .user_data_dir(profile)
                .respect_https_errors()
                .request_timeout(COMMAND_TIMEOUT)
                .launch_timeout(COMMAND_TIMEOUT);
            if !headless {
                config = config.with_head();
            }
            let config = config
                .build()
                .map_err(|error| anyhow!("building browser configuration: {error}"))?;
            let (browser, handler) = timeout(COMMAND_TIMEOUT, Chromium::launch(config))
                .await
                .context("launching Chromium timed out")??;
            (browser, handler, true)
        };
        let handler = tokio::spawn(async move {
            let mut handler = handler;
            while let Some(event) = handler.next().await {
                event.context("Chromium event handler failed")?;
            }
            Ok(())
        });
        Ok(Self {
            chromium: browser,
            handler,
            owned,
        })
    }

    #[tracing::instrument(skip_all)]
    pub async fn fetch(
        &self,
        target: &Target,
        headers: &BTreeMap<String, String>,
        interval: Duration,
    ) -> Result<Capture> {
        let page = timeout(COMMAND_TIMEOUT, self.chromium.new_page("about:blank"))
            .await
            .context("opening browser page timed out")??;
        let deadline = interval
            .checked_add(COMMAND_TIMEOUT.saturating_mul(2))
            .context("browser deadline overflow")?;
        let result = timeout(deadline, self.fetch_page(&page, target, headers, interval))
            .await
            .context("browser request timed out")
            .and_then(|result| result);
        let cleanup = timeout(COMMAND_TIMEOUT, page.close())
            .await
            .context("closing browser page timed out")
            .and_then(|result| result.map_err(Into::into));
        finish_page_fetch(result, cleanup)
    }

    async fn fetch_page(
        &self,
        page: &Page,
        target: &Target,
        headers: &BTreeMap<String, String>,
        interval: Duration,
    ) -> Result<Capture> {
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
                return Ok(bootstrap);
            }
            tokio::time::sleep(interval).await;
            return browser_fetch(page, target, headers, method).await;
        }
        if method == "GET" && target.body.is_none() && headers.is_empty() {
            return navigate(page, &target.url).await;
        }
        let url = url::Url::parse(&target.url).context("parsing browser target")?;
        let origin = url.origin().ascii_serialization();
        let bootstrap = navigate(page, &origin).await?;
        if access_stop_status(bootstrap.status)
            || denied_capture(&bootstrap)
            || !(200..300).contains(&bootstrap.status)
        {
            return Ok(bootstrap);
        }
        tokio::time::sleep(interval).await;
        browser_fetch(page, target, headers, method).await
    }

    #[tracing::instrument(skip_all)]
    pub async fn close(mut self) -> Result<()> {
        let close = if self.owned {
            timeout(COMMAND_TIMEOUT, self.chromium.close())
                .await
                .context("closing Chromium timed out")
                .and_then(|result| result.map(|_| ()).map_err(Into::into))
        } else {
            Ok(())
        };
        let waited = if self.owned && close.is_ok() {
            timeout(COMMAND_TIMEOUT, self.chromium.wait())
                .await
                .context("waiting for Chromium timed out")
                .and_then(|result| result.map(|_| ()).map_err(Into::into))
        } else {
            Ok(())
        };
        let killed = if self.owned && (close.is_err() || waited.is_err()) {
            self.chromium
                .kill()
                .await
                .map_or_else(|| Ok(()), |result| result.context("killing Chromium"))
        } else {
            Ok(())
        };
        if !self.owned || close.is_err() || waited.is_err() {
            self.handler.abort();
        }
        let joined = match self.handler.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(anyhow!(error).context("joining Chromium handler")),
        };
        close.and(waited).and(killed).and(joined)
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

async fn navigate(page: &Page, url: &str) -> Result<Capture> {
    let limit = i64::try_from(MAX_BODY_BYTES).context("browser buffer limit")?;
    page.execute(
        network::EnableParams::builder()
            .max_resource_buffer_size(limit)
            .max_total_buffer_size(limit.saturating_mul(2))
            .build(),
    )
    .await?;
    let frame = page
        .mainframe()
        .await?
        .context("browser page has no main frame")?;
    let responses = page.event_listener::<EventResponseReceived>().await?;
    let finished = page.event_listener::<EventLoadingFinished>().await?;
    page.goto(url).await.context("navigating browser page")?;
    let response = responses
        .filter(|event| {
            futures::future::ready(
                event.r#type == ResourceType::Document && event.frame_id.as_ref() == Some(&frame),
            )
        })
        .next()
        .await
        .context("browser navigation ended without a document response")?;
    let status =
        u16::try_from(response.response.status).context("invalid browser response status")?;
    let body_result = timeout(COMMAND_TIMEOUT, async {
        let request_id = response.request_id.clone();
        finished
            .filter(|event| futures::future::ready(event.request_id == request_id))
            .next()
            .await
            .context("browser document did not finish loading")?;
        let result = page
            .execute(GetResponseBodyParams::new(request_id))
            .await
            .context("capturing browser document")?
            .result;
        let body = if result.base64_encoded {
            decode_body(&result.body)?
        } else {
            result.body.into_bytes()
        };
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "source response exceeded 16 MiB limit"
        );
        Ok::<_, anyhow::Error>(body)
    })
    .await
    .context("capturing browser document timed out")
    .and_then(|result| result);
    let body = body_result.map_err(|error| {
        if access_stop_status(status) || denied_url(&response.response.url) {
            AccessStopCaptureError { status, error }.into()
        } else {
            error
        }
    })?;
    let capture = Capture {
        url: response.response.url.clone(),
        status,
        content_type: response.response.mime_type.clone(),
        body,
    };
    if !capture.content_type.contains("html")
        || !(200..300).contains(&capture.status)
        || denied_capture(&capture)
    {
        return Ok(capture);
    }
    let rendered: String = page
        .evaluate_function(RENDER_SCRIPT)
        .await
        .context("capturing rendered page")?
        .into_value()
        .context("decoding rendered page")?;
    Ok(Capture {
        body: decode_body(&rendered)?,
        ..capture
    })
}

const RENDER_SCRIPT: &str = r"async function() {
    if (document.readyState !== 'complete') await new Promise(resolve => window.addEventListener('load', resolve, {once: true}));
    const bytes = new TextEncoder().encode(document.documentElement.outerHTML);
    if (bytes.length > 16777216) throw new Error('source response exceeded 16 MiB limit');
    let binary = '';
    for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
    return btoa(binary);
}";

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
        let browser = Browser::start(None, profile.path(), true).await?;
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
            let redirected = browser.fetch(&target, &headers, Duration::ZERO).await?;
            assert!(redirected.url.ends_with("/subscribe"));
            let failure = check_response(redirected).expect_err("subscription must stop");
            assert!(failure.blocked);
            assert_eq!(api_requests.load(Ordering::SeqCst), before);
            target.method = None;
            target.body = None;
            target.browser_origin = None;
            target.url = format!("{origin}/redirect-login");
            let redirected = browser
                .fetch(&target, &BTreeMap::new(), Duration::ZERO)
                .await?;
            assert!(redirected.url.ends_with("/login"));
            assert!(
                check_response(redirected)
                    .expect_err("login must stop")
                    .blocked
            );
            target.browser_origin = Some(origin.clone());
            target.url = format!("{origin}/api-redirect");
            let redirected = browser.fetch(&target, &headers, Duration::ZERO).await?;
            assert!(redirected.url.ends_with("/signin"));
            assert!(
                check_response(redirected)
                    .expect_err("signin must stop")
                    .blocked
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let closed = browser.close().await;
        server.abort();
        scenario.and(closed)
    }
}
