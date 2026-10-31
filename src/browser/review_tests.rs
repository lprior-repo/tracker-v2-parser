use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, oneshot},
    task::JoinHandle,
    time::timeout,
};

use super::Browser;
use crate::model::{PageContext, PageKind, Source, Target};

mod protocol;
mod regressions;

type Routes = BTreeMap<String, (u16, String, String, String)>;

struct Fixture {
    origin: String,
    requests: Arc<Mutex<Vec<String>>>,
    received: Arc<Mutex<Vec<(String, tokio::time::Instant)>>>,
    connections: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    changed: Arc<Notify>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<()>>,
}

impl Fixture {
    async fn start(routes: Routes) -> Result<Self> {
        Self::start_with_gate(routes, None).await
    }

    async fn start_with_gate(routes: Routes, gate: Option<Arc<Notify>>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&requests);
        let received = Arc::new(Mutex::new(Vec::new()));
        let timestamps = Arc::clone(&received);
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::clone(&connections);
        let active = Arc::new(AtomicUsize::new(0));
        let running = Arc::clone(&active);
        let changed = Arc::new(Notify::new());
        let notifications = Arc::clone(&changed);
        let routes = Arc::new(routes);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            stream::try_unfold((listener, accepted), |(listener, accepted)| async move {
                let (socket, _) = listener.accept().await?;
                accepted.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::io::Error>(Some((socket, (listener, accepted))))
            })
            .take_until(stopped)
            .take(1024)
            .map_err(anyhow::Error::from)
            .map_ok(|socket| {
                serve(
                    socket,
                    Arc::clone(&routes),
                    Arc::clone(&observed),
                    Arc::clone(&timestamps),
                    Arc::clone(&running),
                    Arc::clone(&notifications),
                    gate.clone(),
                )
            })
            .try_buffer_unordered(8)
            .try_collect::<Vec<_>>()
            .await?;
            Ok(())
        });
        Ok(Self {
            origin,
            requests,
            received,
            connections,
            active,
            changed,
            stop,
            task,
        })
    }

    fn recorded(&self) -> Result<Vec<String>> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    async fn wait_for_request(&self, prefix: &str) -> Result<()> {
        timeout(Duration::from_secs(5), async {
            for _ in 0..1024 {
                if self
                    .recorded()?
                    .iter()
                    .any(|request| request.starts_with(prefix))
                {
                    return Ok(());
                }
                self.changed.notified().await;
            }
            Err(anyhow::anyhow!(
                "fixture request notification budget exceeded"
            ))
        })
        .await
        .context("waiting for fixture request timed out")?
    }

    async fn wait_for_idle(&self) -> Result<()> {
        timeout(Duration::from_secs(5), async {
            for _ in 0..1024 {
                if self.active.load(Ordering::SeqCst) == 0 {
                    return Ok(());
                }
                self.changed.notified().await;
            }
            Err(anyhow::anyhow!(
                "fixture connection notification budget exceeded"
            ))
        })
        .await
        .context("waiting for fixture connection cleanup timed out")?
    }

    async fn close(mut self) -> Result<()> {
        if self.stop.send(()).is_err() {
            tracing::debug!("fixture already stopped");
        }
        match timeout(Duration::from_secs(3), &mut self.task).await {
            Ok(result) => result?,
            Err(_) => {
                self.task.abort();
                match self.task.await {
                    Err(error) if error.is_cancelled() => Ok(()),
                    Err(error) => Err(error.into()),
                    Ok(result) => result,
                }
            }
        }
    }
}

async fn serve(
    mut socket: TcpStream,
    routes: Arc<Routes>,
    requests: Arc<Mutex<Vec<String>>>,
    received: Arc<Mutex<Vec<(String, tokio::time::Instant)>>>,
    active: Arc<AtomicUsize>,
    changed: Arc<Notify>,
    gate: Option<Arc<Notify>>,
) -> Result<()> {
    active.fetch_add(1, Ordering::SeqCst);
    let _connection = Connection {
        active,
        changed: Arc::clone(&changed),
    };
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    for _ in 0..4096 {
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8(bytes)?;
    let path = request
        .split_whitespace()
        .nth(1)
        .context("fixture request omitted path")?;
    received
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .push((path.to_owned(), tokio::time::Instant::now()));
    requests
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .push(request.clone());
    changed.notify_one();
    if path == "/capture-ready"
        && let Some(gate) = gate
    {
        timeout(Duration::from_secs(15), gate.notified())
            .await
            .context("fixture capture-readiness gate timed out")?;
    }
    if path == "/large-challenge" {
        socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 16777217\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    }
    if matches!(path, "/hang" | "/hang-denied" | "/hang-throttled") {
        let status = match path {
            "/hang-denied" => 403,
            "/hang-throttled" => 429,
            _ => 200,
        };
        socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: text/html\r\nContent-Length: 99999\r\nConnection: keep-alive\r\n\r\n").as_bytes()).await?;
        match socket.read(&mut buffer).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    let fallback = (
        200,
        "text/plain".to_owned(),
        "canary".to_owned(),
        String::new(),
    );
    let (status, kind, body, headers) = routes.get(path).map_or(&fallback, |route| route);
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}",
        body.len()
    );
    if let Err(error) = socket.write_all(response.as_bytes()).await {
        if !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ) {
            return Err(error.into());
        }
    }
    Ok(())
}

struct Connection {
    active: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.changed.notify_one();
    }
}

fn html(body: String) -> (u16, String, String, String) {
    (
        200,
        "text/html".to_owned(),
        format!("<html><body>{body}</body></html>"),
        String::new(),
    )
}

fn target(origin: &str, path: &str) -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        format!("{origin}{path}"),
        PageContext::default(),
    )
}

async fn browser(origin: &str, profile: &std::path::Path) -> Result<Browser> {
    let mut browser = Browser::start(None, profile, true).await?;
    browser.local_origin = Some(url::Url::parse(origin)?.origin());
    Ok(browser)
}

#[tokio::test]
async fn given_forbidden_redirect_when_chromium_navigates_then_destination_receives_zero_requests()
-> Result<()> {
    let forbidden = Fixture::start(BTreeMap::new()).await?;
    let safe = Fixture::start(BTreeMap::from([(
        "/redirect".into(),
        (
            302,
            "text/plain".into(),
            String::new(),
            format!("Location: {}/secret\r\n", forbidden.origin),
        ),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let capture = browser
            .fetch(
                &target(&safe.origin, "/redirect"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await?;
        assert_eq!(capture.status, 403);
        assert!(forbidden.recorded()?.is_empty());
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /redirect "))
        );
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    let forbidden_closed = forbidden.close().await;
    scenario.and(closed).and(safe_closed).and(forbidden_closed)
}

#[tokio::test]
async fn given_rendered_requests_when_chromium_fetches_subresources_then_forbidden_server_receives_zero_requests()
-> Result<()> {
    let forbidden = Fixture::start(BTreeMap::new()).await?;
    let body = format!(
        "<script>fetch('/executed'); fetch('{0}/fetch').catch(() => {{}})</script><img src='{0}/image'><script src='/jump'></script>",
        forbidden.origin
    );
    let safe = Fixture::start(BTreeMap::from([
        ("/page".into(), html(body)),
        (
            "/jump".into(),
            (
                302,
                "text/plain".into(),
                String::new(),
                format!("Location: {}/script\r\n", forbidden.origin),
            ),
        ),
    ]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let result = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await;
        assert!(result.is_err());
        assert!(forbidden.recorded()?.is_empty());
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /executed "))
        );
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /jump "))
        );
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    let forbidden_closed = forbidden.close().await;
    scenario.and(closed).and(safe_closed).and(forbidden_closed)
}

#[tokio::test]
async fn given_safe_script_and_cookie_when_chromium_renders_then_real_dom_and_cookie_requests_are_preserved()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([
        ("/page".into(), (201, "text/html".into(), "<html><body><script src='/render.js'></script></body></html>".into(), "Set-Cookie: fixture=present; Path=/; SameSite=Lax\r\n".into())),
        ("/render.js".into(), (200, "text/javascript".into(), "document.body.insertAdjacentHTML('beforeend','<p id=rendered>real browser '+document.cookie+'</p>')".into(), String::new())),
    ])).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let capture = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await?;
        assert_eq!(capture.status, 201);
        assert!(String::from_utf8(capture.body)?.contains("real browser fixture=present"));
        assert!(safe.recorded()?.iter().any(|request| {
            request.starts_with("GET /render.js ")
                && request
                    .to_ascii_lowercase()
                    .contains("cookie: fixture=present")
        }));
        let remaining = browser.chromium.pages().await?;
        let identities: Vec<_> = remaining
            .iter()
            .map(|entry| (&entry.target_id, &entry.r#type))
            .collect();
        assert_eq!(
            remaining.len(),
            initial_pages,
            "actual owned Chromium targets: {identities:?}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

async fn auxiliary_is_blocked(script: &str) -> Result<()> {
    let forbidden = Fixture::start(BTreeMap::new()).await?;
    let script = script
        .replace("CANARY", &forbidden.origin)
        .replace("WEBSOCKET", &forbidden.origin.replacen("http:", "ws:", 1))
        .replace(
            "SECURE_SOCKET",
            &forbidden.origin.replacen("http:", "wss:", 1),
        );
    let safe = Fixture::start(BTreeMap::from([("/page".into(), html(script))])).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let result = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await;
        ensure!(
            result.is_err(),
            "auxiliary-context acquisition must be explicitly incomplete"
        );
        assert!(forbidden.recorded()?.is_empty());
        assert_eq!(forbidden.connections.load(Ordering::SeqCst), 0);
        assert!(
            !safe
                .recorded()?
                .iter()
                .any(|request| request.starts_with("GET /worker "))
        );
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    let forbidden_closed = forbidden.close().await;
    scenario.and(closed).and(safe_closed).and(forbidden_closed)
}

#[tokio::test]
async fn given_worker_when_chromium_creates_context_then_no_worker_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<script>try { new Worker('/worker') } catch (_) {}</script>").await
}

#[tokio::test]
async fn given_service_worker_when_chromium_registers_then_no_worker_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked(
        "<script>navigator.serviceWorker.register('/worker').catch(() => {})</script>",
    )
    .await
}

#[tokio::test]
async fn given_frame_when_chromium_embeds_then_no_destination_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<iframe src='CANARY/frame'></iframe>").await
}

#[tokio::test]
async fn given_websocket_when_chromium_connects_then_no_destination_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<script>try { new WebSocket('WEBSOCKET/socket') } catch (_) {}</script>")
        .await
}

#[tokio::test]
async fn given_popup_when_chromium_opens_then_no_destination_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<script>window.open('CANARY/popup')</script>").await
}

#[tokio::test]
async fn given_shared_worker_when_chromium_creates_context_then_no_worker_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<script>try { new SharedWorker('/worker') } catch (_) {}</script>").await
}

#[tokio::test]
async fn given_targeted_link_when_chromium_opens_then_no_destination_request_and_explicit_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<a id=popup target=_blank href='CANARY/popup'>popup</a><script>document.getElementById('popup').click()</script>").await
}

#[tokio::test]
async fn given_timeout_and_error_when_fetch_finishes_then_pages_and_owned_interception_are_drained()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([(
        "/page".into(),
        html("<p>recovered</p>".into()),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        browser.page_deadline = Some(Duration::from_millis(500));
        let timed_out = timeout(
            Duration::from_secs(12),
            browser.fetch(
                &target(&safe.origin, "/hang"),
                &BTreeMap::new(),
                Duration::ZERO,
            ),
        )
        .await?;
        assert!(timed_out.is_err());
        assert!(
            format!("{:#}", timed_out.err().context("expected timeout")?)
                .contains("browser request timed out")
        );
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        browser.page_deadline = None;
        let mut invalid = target(&safe.origin, "/page");
        invalid.method = Some("DELETE".into());
        assert!(
            browser
                .fetch(&invalid, &BTreeMap::new(), Duration::ZERO)
                .await
                .is_err()
        );
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        let recovered = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await?;
        assert!(String::from_utf8(recovered.body)?.contains("<p>recovered</p>"));
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_authentication_redirect_when_chromium_navigates_then_login_receives_zero_requests()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([(
        "/redirect".into(),
        (
            302,
            "text/plain".into(),
            String::new(),
            "Location: /login\r\n".into(),
        ),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let scenario = async {
        let error = browser
            .fetch(
                &target(&safe.origin, "/redirect"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await
            .expect_err("login redirect must preserve a typed stop");
        let failure = crate::transport::failure(&error);
        assert!(failure.blocked);
        assert!(failure.capture.is_none());
        assert_eq!(
            failure.stopped_origin.as_deref(),
            Some(safe.origin.as_str())
        );
        let requests = safe.recorded()?;
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with("GET /redirect "))
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.starts_with("GET /login "))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_javascript_navigation_when_chromium_redirects_then_forbidden_destination_receives_zero_requests()
-> Result<()> {
    let forbidden = Fixture::start(BTreeMap::new()).await?;
    let safe = Fixture::start(BTreeMap::from([(
        "/page".into(),
        html(format!(
            "<script>location.assign('{}/navigation')</script>",
            forbidden.origin
        )),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let scenario = async {
        let result = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await;
        if let Ok(capture) = result {
            assert_eq!(capture.status, 403);
        }
        assert!(forbidden.recorded()?.is_empty());
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /page "))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    let forbidden_closed = forbidden.close().await;
    scenario.and(closed).and(safe_closed).and(forbidden_closed)
}

#[tokio::test]
async fn given_peer_connection_when_chromium_creates_transport_then_acquisition_is_explicitly_incomplete()
-> Result<()> {
    auxiliary_is_blocked("<script>try { new RTCPeerConnection({iceServers: [{urls: 'stun:127.0.0.1:3478'}]}) } catch (_) {}</script>").await
}

#[tokio::test]
async fn given_oversized_challenge_when_chromium_captures_then_access_stop_survives_capture_failure()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::new()).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let error = browser
            .fetch(
                &target(&safe.origin, "/large-challenge"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await
            .err()
            .context("oversized challenge must fail capture")?;
        let stop = error
            .downcast_ref::<crate::transport::AccessStopCaptureError>()
            .context("challenge status must remain typed")?;
        assert_eq!(stop.status, 403);
        assert_eq!(
            crate::transport::failure(&error).stopped_origin.as_deref(),
            Some(safe.origin.as_str())
        );
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_secure_websocket_when_chromium_connects_then_private_destination_receives_zero_connections()
-> Result<()> {
    auxiliary_is_blocked(
        "<script>try { new WebSocket('SECURE_SOCKET/socket') } catch (_) {}</script>",
    )
    .await
}

#[tokio::test]
async fn given_alternate_realm_websocket_when_chromium_connects_then_destination_receives_zero_connections()
-> Result<()> {
    auxiliary_is_blocked("<script>const frame = document.createElement('iframe'); document.body.append(frame); try { new frame.contentWindow.WebSocket('WEBSOCKET/socket') } catch (_) {}</script>").await
}

#[tokio::test]
async fn given_alternate_realm_secure_websocket_when_chromium_connects_then_destination_receives_zero_connections()
-> Result<()> {
    auxiliary_is_blocked("<script>const frame = document.createElement('iframe'); document.body.append(frame); try { new frame.contentWindow.WebSocket('SECURE_SOCKET/socket') } catch (_) {}</script>").await
}

#[tokio::test]
async fn given_external_fetch_cancellation_when_caller_drops_then_owned_tasks_pages_and_connections_are_drained()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::new()).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let target = target(&safe.origin, "/hang");
        let headers = BTreeMap::new();
        let mut pending = Box::pin(browser.fetch(&target, &headers, Duration::ZERO));
        tokio::select! {
            result = &mut pending => return Err(anyhow::anyhow!("fetch ended before controlled cancellation: {result:?}")),
            observed = safe.wait_for_request("GET /hang ") => observed?,
        }
        drop(pending);
        let mut tasks = browser.fetches.lock().await;
        timeout(Duration::from_secs(15), super::lifecycle::drain(&mut tasks)).await??;
        assert!(tasks.is_empty());
        drop(tasks);
        safe.wait_for_idle().await?;
        assert_eq!(safe.active.load(Ordering::SeqCst), 0);
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }.await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_unfinished_subresource_when_drain_deadline_expires_then_acquisition_is_incomplete_not_success()
-> Result<()> {
    let release = Arc::new(Notify::new());
    let safe = Fixture::start_with_gate(
        BTreeMap::from([
            ("/page".into(), html("<script>fetch('/hang').catch(() => {})</script><script src='/capture-ready'></script>".into())),
            ("/capture-ready".into(), (200, "text/javascript".into(), String::new(), String::new())),
        ]),
        Some(Arc::clone(&release)),
    )
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let target = target(&safe.origin, "/page");
        let headers = BTreeMap::new();
        let mut pending = Box::pin(browser.fetch(&target, &headers, Duration::ZERO));
        tokio::select! {
            result = &mut pending => return Err(anyhow::anyhow!("fetch ended before hanging subresource was admitted: {result:?}")),
            observed = safe.wait_for_request("GET /hang ") => observed?,
        }
        let ready = safe.wait_for_request("GET /capture-ready ").await;
        release.notify_one();
        ready?;
        let error = match pending.await {
            Err(error) => error,
            Ok(capture) => return Err(anyhow::anyhow!(
                "forced drain must not return successful capture: status={}, url={}, body={}",
                capture.status, capture.url, String::from_utf8_lossy(&capture.body),
            )),
        };
        let detail = format!("{error:#}");
        assert!(detail.contains("acquisition incomplete"), "{detail}");
        assert!(detail.contains("interception exceeded drain deadline"), "{detail}");
        assert!(browser.fetches.lock().await.is_empty());
        safe.wait_for_idle().await?;
        assert_eq!(safe.active.load(Ordering::SeqCst), 0);
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        Ok::<_, anyhow::Error>(())
    }.await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_worker_violation_then_navigation_when_chromium_captures_then_acquisition_stays_incomplete()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([
        ("/page".into(), html("<script>try { new Worker('/worker') } catch (_) {} location.replace('/canary')</script>".into())),
        ("/canary".into(), html("<p>otherwise valid final page</p>".into())),
    ])).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let scenario = async {
        let error = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await
            .err()
            .context("navigation must not reset acquisition violation")?;
        assert!(format!("{error:#}").contains("blocked"));
        let requests = safe.recorded()?;
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with("GET /canary "))
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.starts_with("GET /worker "))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_custom_credentials_when_browser_crosses_origins_then_allowed_peer_never_receives_credentials_or_preflight()
-> Result<()> {
    let peer = Fixture::start(BTreeMap::new()).await?;
    let safe = Fixture::start(BTreeMap::from([
        ("/control".into(), html(format!("<img src='{}/control'>", peer.origin))),
        ("/secret".into(), html(format!("<script>fetch('{}/secret', {{headers: {{'x-auth-token': 'origin-bound-secret'}}}}).catch(() => {{}})</script>", peer.origin))),
        ("/redirect".into(), (302, "text/plain".into(), String::new(), format!("Location: {}/redirected\r\n", peer.origin))),
    ])).await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&safe.origin, profile.path()).await?;
    browser.additional_origin = Some(url::Url::parse(&peer.origin)?.origin());
    let scenario = async {
        browser
            .fetch(
                &target(&safe.origin, "/control"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await?;
        assert!(
            peer.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /control "))
        );
        let baseline = peer.connections.load(Ordering::SeqCst);
        assert!(
            browser
                .fetch(
                    &target(&safe.origin, "/secret"),
                    &BTreeMap::new(),
                    Duration::ZERO
                )
                .await
                .is_err()
        );
        assert_eq!(peer.connections.load(Ordering::SeqCst), baseline);
        let mut redirected = target(&safe.origin, "/redirect");
        redirected.browser_origin = Some(safe.origin.clone());
        let headers = BTreeMap::from([("x-auth-token".into(), "caller-bound-secret".into())]);
        if let Ok(capture) = browser.fetch(&redirected, &headers, Duration::ZERO).await {
            assert_eq!(capture.status, 403);
        }
        assert_eq!(peer.connections.load(Ordering::SeqCst), baseline);
        assert!(
            !peer
                .recorded()?
                .iter()
                .any(|request| request.contains("origin-bound-secret")
                    || request.contains("caller-bound-secret"))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    let peer_closed = peer.close().await;
    scenario.and(closed).and(safe_closed).and(peer_closed)
}

#[tokio::test]
async fn given_not_modified_response_when_browser_fetches_then_status_is_preserved_without_redirect_location()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([(
        "/not-modified".into(),
        (304, "application/json".into(), String::new(), String::new()),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let scenario = async {
        let mut target = target(&safe.origin, "/not-modified");
        target.browser_origin = Some(safe.origin.clone());
        let capture = browser
            .fetch(&target, &BTreeMap::new(), Duration::ZERO)
            .await?;
        assert_eq!(capture.status, 304);
        assert!(capture.body.is_empty());
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_external_driver_when_starting_browser_then_unprotected_remote_acquisition_is_rejected()
-> Result<()> {
    let profile = tempfile::tempdir()?;
    let error = Browser::start(Some("http://127.0.0.1:9222"), profile.path(), true)
        .await
        .err()
        .context("external driver must be explicitly rejected")?;
    assert!(format!("{error:#}").contains("requires locally owned Chromium"));
    Ok(())
}

async fn native_websocket_is_sink_routed(secure: bool) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::{
        network::{EventWebSocketClosed, EventWebSocketCreated},
        target::CreateTargetParams,
    };

    let forbidden = Fixture::start(BTreeMap::new()).await?;
    let profile = tempfile::tempdir()?;
    let browser = Browser::start(None, profile.path(), true).await?;
    let scenario = async {
        let context = browser
            .context
            .clone()
            .context("protected browser context missing")?;
        let params = CreateTargetParams::builder()
            .url("about:blank")
            .browser_context_id(context)
            .build()
            .map_err(anyhow::Error::msg)?;
        let page = browser.chromium.new_page(params).await?;
        let scenario = async {
            let created = page.event_listener::<EventWebSocketCreated>().await?;
            let closed = page.event_listener::<EventWebSocketClosed>().await?;
            let scheme = if secure { "wss:" } else { "ws:" };
            let url = url::Url::parse(&forbidden.origin.replacen("http:", scheme, 1))?.to_string();
            page.evaluate_expression(format!(
                "new WebSocket({}); true",
                serde_json::to_string(&url)?
            ))
            .await?;
            let event = timeout(
                Duration::from_secs(5),
                created
                    .filter(|event| futures::future::ready(event.url == url))
                    .next(),
            )
            .await.context("native WebSocket creation event timed out")?
            .context("native WebSocket creation event missing")?;
            timeout(
                Duration::from_secs(5),
                closed
                    .filter(|closed| futures::future::ready(closed.request_id == event.request_id))
                    .next(),
            )
            .await.with_context(|| format!(
                "native WebSocket close event timed out (accepted/probes/rejected_connects={:?}, denied={})",
                browser.native_sink.status().counts(), browser.native_sink.denied_connections(),
            ))?
            .context("native WebSocket did not close at deny sink")?;
            browser.native_sink.status().checkpoint().await?;
            assert!(browser.native_sink.denied_connections() > 0);
            assert_eq!(forbidden.connections.load(Ordering::SeqCst), 0);
            assert!(forbidden.recorded()?.is_empty());
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let closed = page.close().await.map_err(anyhow::Error::from);
        scenario.and(closed)
    }
    .await;
    let closed = browser.close().await;
    let forbidden_closed = forbidden.close().await;
    scenario.and(closed).and(forbidden_closed)
}

#[tokio::test]
async fn given_unintercepted_native_websocket_when_chromium_connects_then_only_owned_sink_receives_connection()
-> Result<()> {
    native_websocket_is_sink_routed(false).await
}

#[tokio::test]
async fn given_unintercepted_native_secure_websocket_when_chromium_connects_then_only_owned_sink_receives_connection()
-> Result<()> {
    native_websocket_is_sink_routed(true).await
}

#[tokio::test]
async fn given_if_auth_token_and_accepting_preflight_when_browser_crosses_origins_then_peer_receives_no_secret_request()
-> Result<()> {
    let peer = Fixture::start(BTreeMap::from([(
        "/conditional".into(),
        (200, "application/json".into(), "{}".into(),
         "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: If-Auth-Token\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\n".into()),
    )])).await?;
    let safe = Fixture::start(BTreeMap::from([
        ("/control".into(), html(format!("<img src='{}/control'>", peer.origin))),
        ("/page".into(), html(format!("<script>fetch('{}/conditional', {{headers: {{'If-Auth-Token': 'conditional-secret'}}}}).catch(() => {{}})</script>", peer.origin))),
    ])).await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&safe.origin, profile.path()).await?;
    browser.additional_origin = Some(url::Url::parse(&peer.origin)?.origin());
    let scenario = async {
        browser
            .fetch(
                &target(&safe.origin, "/control"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await?;
        assert!(
            peer.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /control "))
        );
        let baseline = peer.connections.load(Ordering::SeqCst);
        let result = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await;
        assert!(result.is_err());
        assert_eq!(peer.connections.load(Ordering::SeqCst), baseline);
        assert!(
            !peer
                .recorded()?
                .iter()
                .any(|request| request.contains("conditional-secret")
                    || request.contains("/conditional "))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    let peer_closed = peer.close().await;
    scenario.and(closed).and(safe_closed).and(peer_closed)
}

#[tokio::test]
async fn given_delayed_csp_issue_and_navigation_when_target_tears_down_then_ordered_audit_preserves_incomplete()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([
        ("/page".into(), html("<script>addEventListener('securitypolicyviolation', event => event.stopImmediatePropagation(), true); const frame = document.createElement('iframe'); frame.src = '/blocked-frame'; document.body.append(frame); location.replace('/canary')</script>".into())),
        ("/canary".into(), html("<p>valid final page cannot erase prior CSP denial</p>".into())),
    ])).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let scenario = async {
        let result = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await;
        assert!(result.is_err());
        let requests = safe.recorded()?;
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with("GET /canary "))
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.starts_with("GET /blocked-frame "))
        );
        assert!(browser.fetches.lock().await.is_empty());
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_page_spoofed_completion_when_acquiring_then_page_cannot_certify_audit_teardown()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([("/page".into(), html(
        "<script>__trackBlockedBinding('barrier')</script><p>page attempted to forge completion</p>".into(),
    ))])).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let scenario = async {
        let result = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await;
        assert!(result.is_err());
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /page "))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_page_close_failure_when_acquisition_finishes_then_context_is_disposed_before_interception_stops()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([(
        "/page".into(),
        html("<p>real page must be torn down</p>".into()),
    )]))
    .await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    browser.fail_page_close = true;
    let scenario = async {
        let error = browser
            .fetch(
                &target(&safe.origin, "/page"),
                &BTreeMap::new(),
                Duration::ZERO,
            )
            .await
            .err()
            .context("failed page close must not return successful capture")?;
        assert!(format!("{error:#}").contains("page-close failure"));
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        assert!(browser.fetches.lock().await.is_empty());
        safe.wait_for_idle().await?;
        assert_eq!(safe.active.load(Ordering::SeqCst), 0);
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /page "))
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_received_unobserved_native_request_when_checkpoint_runs_then_denial_is_recorded_before_acknowledgement()
-> Result<()> {
    let sink = super::native::NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let scenario = async {
        let connected = async {
            let mut socket = TcpStream::connect(sink.address()?).await?;
            socket.write_all(b"G").await?;
            Ok::<_, anyhow::Error>(socket)
        };
        let (release, mut socket) = tokio::try_join!(sink.pause_after_prefix(b'G'), connected)?;
        assert_eq!(sink.denied_connections(), 0);
        let status = sink.status();
        assert_eq!(status.counts(), (1, 0, 0));
        let mut checkpoint = Box::pin(status.checkpoint());
        assert!(futures::poll!(checkpoint.as_mut()).is_pending());
        release
            .send(())
            .map_err(|()| anyhow::anyhow!("sink pause owner disappeared"))?;
        assert_eq!(checkpoint.await?, 1);
        assert_eq!(sink.denied_connections(), 1);
        assert_eq!(status.counts(), (1, 0, 0));
        assert_eq!(status.denial_kinds(), (1, 0, 0));
        let mut byte = [0; 1];
        assert_eq!(
            timeout(Duration::from_secs(5), socket.read(&mut byte)).await??,
            0
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = sink.finish().await;
    scenario.and(closed)
}

#[tokio::test]
async fn given_empty_speculative_socket_when_teardown_checkpoint_runs_then_probe_does_not_make_acquisition_incomplete()
-> Result<()> {
    let sink = super::native::NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let scenario = async {
        let release = sink.pause().await?;
        let mut socket = TcpStream::connect(sink.address()?).await?;
        let status = sink.status();
        let mut checkpoint = Box::pin(status.checkpoint());
        assert!(futures::poll!(checkpoint.as_mut()).is_pending());
        release
            .send(())
            .map_err(|()| anyhow::anyhow!("sink pause owner disappeared"))?;
        assert_eq!(checkpoint.await?, 0);
        assert_eq!(status.counts(), (1, 1, 0));
        assert_eq!(sink.denied_connections(), 0);
        let mut byte = [0; 1];
        assert_eq!(
            timeout(Duration::from_secs(5), socket.read(&mut byte)).await??,
            0
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = sink.finish().await;
    scenario.and(closed)
}

#[tokio::test]
async fn given_source_approved_public_connect_when_native_probe_is_closed_then_no_tunnel_is_opened_and_probe_is_not_a_request()
-> Result<()> {
    let sink = super::native::NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let scenario = async {
        let status = sink.status();
        assert_eq!(status.begin(Vec::new()).await?, 0);
        let release = sink.pause().await?;
        let mut socket = TcpStream::connect(sink.address()?).await?;
        socket.write_all(b"CONNECT 8.8.8.8:443 HTTP/1.1\r\nProxy-Authorization: secret-not-inspected\r\n\r\n").await?;
        let mut checkpoint = Box::pin(status.checkpoint());
        assert!(futures::poll!(checkpoint.as_mut()).is_pending());
        release.send(()).map_err(|()| anyhow::anyhow!("sink pause owner disappeared"))?;
        assert_eq!(checkpoint.await?, 0);
        assert_eq!(status.counts(), (1, 1, 0));
        let mut byte = [0; 1];
        let read = timeout(Duration::from_secs(5), socket.read(&mut byte)).await?;
        assert!(matches!(read, Ok(0)) || read.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset));
        Ok::<_, anyhow::Error>(())
    }.await;
    let closed = sink.finish().await;
    scenario.and(closed)
}

#[tokio::test]
async fn given_private_connect_when_native_probe_is_closed_then_request_is_denied_even_for_contacts()
-> Result<()> {
    let sink = super::native::NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let scenario = async {
        let status = sink.status();
        status.begin(Vec::new()).await?;
        let release = sink.pause().await?;
        let mut socket = TcpStream::connect(sink.address()?).await?;
        socket
            .write_all(b"CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n")
            .await?;
        let mut checkpoint = Box::pin(status.checkpoint());
        assert!(futures::poll!(checkpoint.as_mut()).is_pending());
        release
            .send(())
            .map_err(|()| anyhow::anyhow!("sink pause owner disappeared"))?;
        assert_eq!(checkpoint.await?, 1);
        assert_eq!(status.counts(), (1, 0, 1));
        assert_eq!(sink.denied_connections(), 1);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = sink.finish().await;
    scenario.and(closed)
}

#[tokio::test]
async fn given_implicit_ui_favicon_when_browser_settles_then_icon_is_pinned_before_target_closes_and_published_dom_is_unchanged()
-> Result<()> {
    let safe = Fixture::start(BTreeMap::from([
        ("/page".into(), html("<title>Ordinary school</title><p>source school</p><a href='/roster'>Roster</a>".into())),
        ("/favicon.ico".into(), (200, "image/svg+xml".into(), "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect width='16' height='16'/></svg>".into(), String::new())),
    ])).await?;
    let profile = tempfile::tempdir()?;
    let browser = browser(&safe.origin, profile.path()).await?;
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let target = target(&safe.origin, "/page");
        for _ in 0..2 {
            let capture = browser
                .fetch(&target, &BTreeMap::new(), Duration::ZERO)
                .await?;
            assert_eq!(capture.status, 200);
            assert_eq!(capture.url, target.url);
            let document = std::str::from_utf8(&capture.body)?;
            assert!(document.contains("source school"));
            assert!(document.contains("href=\"/roster\""));
            assert!(!document.contains("data:image"));
            assert!(browser.fetches.lock().await.is_empty());
            assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        }
        assert!(
            safe.recorded()?
                .iter()
                .any(|request| request.starts_with("GET /favicon.ico "))
        );
        assert_eq!(browser.native_sink.denied_connections(), 0);
        safe.wait_for_idle().await?;
        assert_eq!(safe.active.load(Ordering::SeqCst), 0);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}

#[tokio::test]
async fn given_fetch_started_after_load_when_settlement_completes_then_navigation_captures_updated_dom_and_api_keeps_raw_json()
-> Result<()> {
    let release = Arc::new(Notify::new());
    let captured = Arc::new(Notify::new());
    let safe = Fixture::start_with_gate(
        BTreeMap::from([
            ("/page".into(), html("<p id='record'>Loading</p><script>addEventListener('load', async () => { const response = await fetch('/capture-ready'); document.getElementById('record').textContent = await response.text(); });</script>".into())),
            ("/capture-ready".into(), (200, "text/plain".into(), "Published after-load coaching record".into(), String::new())),
            ("/api".into(), (200, "application/json".into(), "{\"value\":\"raw API response\"}".into(), String::new())),
        ]),
        Some(Arc::clone(&release)),
    ).await?;
    let profile = tempfile::tempdir()?;
    let mut browser = browser(&safe.origin, profile.path()).await?;
    browser.capture_ready = Some(Arc::clone(&captured));
    let initial_pages = browser.chromium.pages().await?.len();
    let scenario = async {
        let target = target(&safe.origin, "/page");
        let headers = BTreeMap::new();
        let mut pending = Box::pin(browser.fetch(&target, &headers, Duration::ZERO));
        tokio::select! {
            result = &mut pending => return Err(anyhow::anyhow!("capture ended before delayed response was released: {result:?}")),
            observed = safe.wait_for_request("GET /capture-ready ") => observed?,
        }
        let ready = timeout(Duration::from_secs(5), captured.notified()).await;
        release.notify_one();
        ready.context("capture stage was not reached while delayed fetch remained pending")?;
        let capture = pending.await?;
        assert_eq!(capture.status, 200);
        let document = std::str::from_utf8(&capture.body)?;
        assert!(document.contains("<p id=\"record\">Published after-load coaching record</p>"), "{document}");
        let mut api = crate::model::Target::get(Source::AthleticNet, PageKind::Contacts, format!("{}/api", safe.origin), PageContext::default());
        api.browser_origin = Some(safe.origin.clone());
        let response = browser.fetch(&api, &headers, Duration::ZERO).await?;
        assert_eq!(response.body, b"{\"value\":\"raw API response\"}");
        assert!(browser.fetches.lock().await.is_empty());
        assert_eq!(browser.chromium.pages().await?.len(), initial_pages);
        safe.wait_for_idle().await?;
        assert_eq!(safe.active.load(Ordering::SeqCst), 0);
        Ok::<_, anyhow::Error>(())
    }.await;
    let closed = browser.close().await;
    let safe_closed = safe.close().await;
    scenario.and(closed).and(safe_closed)
}
