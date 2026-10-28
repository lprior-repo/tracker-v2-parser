use super::*;
use crate::{
    model::{PageContext, PageKind, Source},
    transport::{Mode, Options},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

struct Fixture {
    url: Url,
    requests: Arc<AtomicUsize>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<()>>,
}

impl Fixture {
    async fn start(status: u16, stall_body: bool) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = Url::parse(&format!("http://{}/fixture", listener.local_addr()?))?;
        let requests = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&requests);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = stopped => Ok(()),
                result = async {
                    for _ in 0..4 {
                        let (mut socket, _) = listener.accept().await?;
                        let mut bytes = [0; 4096];
                        ensure!(socket.read(&mut bytes).await? > 0, "fixture request missing");
                        received.fetch_add(1, Ordering::SeqCst);
                        let (length, body) = if stall_body { (9999, "") } else { (2, "ok") };
                        let response = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: text/plain\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}");
                        socket.write_all(response.as_bytes()).await?;
                        if stall_body {
                            match socket.read(&mut bytes).await {
                                Ok(_) => {}
                                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                                Err(error) => return Err(error.into()),
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                } => result,
            }
        });
        Ok(Self {
            url,
            requests,
            stop,
            task,
        })
    }

    async fn close(self) -> Result<()> {
        let sent = self.stop.send(());
        let result = self.task.await?;
        ensure!(sent.is_ok(), "fixture ended before owned shutdown");
        result
    }
}

fn transport(fixture: &Fixture, rps: f64) -> Result<(Transport, Target)> {
    let mut transport = Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: std::path::PathBuf::new(),
        headless: true,
        rps,
    })?;
    transport.http_test_url = Some(fixture.url.clone());
    transport.clients.insert(
        "http://www.milesplit.com".into(),
        Client::builder().no_proxy().build()?,
    );
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "http://www.milesplit.com/athletes/fixture".into(),
        PageContext::default(),
    );
    Ok((transport, target))
}

#[tokio::test]
async fn given_denial_headers_and_stalled_body_when_deadline_expires_then_failure_stays_blocked()
-> Result<()> {
    for status in [403, 429] {
        denied_body(status).await?;
    }
    Ok(())
}

async fn denied_body(status: u16) -> Result<()> {
    let fixture = Fixture::start(status, true).await?;
    let (mut transport, target) = transport(&fixture, 1.0)?;
    transport.http_test_budget = Some(Duration::from_millis(200));
    let result = timeout(Duration::from_secs(3), transport.fetch(&target)).await;
    let requests = fixture.requests.load(Ordering::SeqCst);
    let closed = transport.close().await;
    let fixture_closed = fixture.close().await;
    closed.and(fixture_closed)?;
    let failure = result
        .context("fixture watchdog expired")?
        .expect_err("stalled body");
    assert!(failure.blocked, "{}", failure.detail);
    assert_eq!(
        failure.stopped_origin.as_deref(),
        Some("http://www.milesplit.com")
    );
    assert!(
        failure.detail.contains(&format!("HTTP {status}")),
        "{}",
        failure.detail
    );
    assert_eq!(requests, 1);
    Ok(())
}

#[tokio::test]
async fn given_legal_slow_pacing_when_next_request_waits_then_network_budget_is_not_spent()
-> Result<()> {
    let fixture = Fixture::start(200, false).await?;
    let (mut transport, target) = transport(&fixture, 0.001)?;
    let scenario = async {
        let first = timeout(Duration::from_secs(3), transport.fetch(&target)).await??;
        ensure!(first.status == 200, "first fixture response failed");
        tokio::time::pause();
        let mut pending = Box::pin(transport.fetch(&target));
        let premature = tokio::select! {
            result = &mut pending => Some(result),
            () = tokio::time::sleep(Duration::from_secs(46)) => None,
        };
        tokio::time::advance(Duration::from_secs(954)).await;
        tokio::time::resume();
        ensure!(
            premature.is_none(),
            "intentional pacing consumed the network deadline: {premature:?}"
        );
        let capture = timeout(Duration::from_secs(3), pending).await??;
        ensure!(capture.body == b"ok", "second fixture response was lost");
        ensure!(
            fixture.requests.load(Ordering::SeqCst) == 2,
            "both paced requests must reach fixture"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = transport.close().await;
    let fixture_closed = fixture.close().await;
    scenario.and(closed).and(fixture_closed)
}
