use super::*;
use crate::{
    model::{PageContext, PageKind, Source},
    transport::{Mode, Options},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::{Instant, timeout},
};

struct RedirectFixture {
    url: Url,
    task: JoinHandle<Result<Vec<Instant>>>,
}

impl RedirectFixture {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = Url::parse(&format!("http://{}/fixture", listener.local_addr()?))?;
        let task = tokio::spawn(async move {
            timeout(Duration::from_secs(8), async move {
                let mut arrivals = Vec::new();
                for index in 0..4 {
                    let (mut socket, _) = listener.accept().await?;
                    let mut buffer = [0; 4096];
                    let size = socket.read(&mut buffer).await?;
                    ensure!(String::from_utf8_lossy(&buffer[..size]).starts_with("GET /fixture "), "unexpected fixture request");
                    arrivals.push(Instant::now());
                    let response = if index < 3 {
                        "HTTP/1.1 302 Found\r\nLocation: /athletes/fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    };
                    socket.write_all(response.as_bytes()).await?;
                }
                Ok::<_, anyhow::Error>(arrivals)
            }).await.context("redirect fixture timed out")?
        });
        Ok(Self { url, task })
    }
}

async fn redirected_wire_rate(rps: f64) -> Result<()> {
    let fixture = RedirectFixture::start().await?;
    let mut transport = Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: Default::default(),
        headless: true,
        rps,
    })?;
    transport.http_test_url = Some(fixture.url);
    transport.clients.insert(
        "http://www.milesplit.com".into(),
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?,
    );
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "http://www.milesplit.com/athletes/fixture".into(),
        PageContext::default(),
    );
    let result = transport.fetch(&target).await;
    let closed = transport.close().await;
    let arrivals = fixture
        .task
        .await
        .context("redirect fixture join failed")??;
    closed?;
    let capture = result?;
    assert_eq!(capture.status, 200);
    assert_eq!(capture.body, b"ok");
    assert_eq!(arrivals.len(), 4);
    let minimum = Duration::from_secs_f64(rps.recip()).saturating_sub(Duration::from_millis(10));
    for pair in arrivals.windows(2) {
        assert!(pair[1].duration_since(pair[0]) >= minimum);
    }
    assert!(
        arrivals[3].duration_since(arrivals[0]) < Duration::from_millis(2500),
        "explicit {rps} RPS was silently limited below the configured rate"
    );
    Ok(())
}

#[tokio::test]
async fn given_two_rps_and_three_redirects_when_fetched_then_each_hop_is_paced_without_the_old_one_rps_cap()
-> Result<()> {
    redirected_wire_rate(2.0).await
}

#[tokio::test]
async fn given_three_rps_and_three_redirects_when_fetched_then_each_hop_is_paced_without_the_old_one_rps_cap()
-> Result<()> {
    redirected_wire_rate(3.0).await
}
