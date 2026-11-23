use std::sync::Arc;

use tokio::{net::TcpStream, sync::Notify};

use super::*;

async fn read_request(socket: &mut TcpStream) -> Result<Instant> {
    let mut bytes = [0; 4096];
    let mut length = 0;
    let mut arrived = None;
    for _ in 0..32 {
        let read = socket.read(&mut bytes[length..]).await?;
        ensure!(read > 0, "fixture request ended before its headers");
        arrived.get_or_insert_with(Instant::now);
        length += read;
        if bytes[..length].windows(4).any(|part| part == b"\r\n\r\n") {
            ensure!(
                bytes[..length].starts_with(b"GET /fixture "),
                "unexpected fixture request"
            );
            return arrived.context("fixture request had no arrival time");
        }
        ensure!(length < bytes.len(), "fixture headers exceeded byte bound");
    }
    anyhow::bail!("fixture headers exceeded read bound")
}

async fn serve(listener: TcpListener) -> Result<Vec<Instant>> {
    let mut arrivals = Vec::new();
    for index in 0..2 {
        let (mut socket, _) = listener.accept().await?;
        arrivals.push(read_request(&mut socket).await?);
        let response = if index == 0 {
            b"HTTP/1.1 302 Found\r\nLocation: /athletes/fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".as_slice()
        };
        socket.write_all(response).await?;
    }
    Ok(arrivals)
}

#[tokio::test(start_paused = true)]
async fn given_delayed_http_preparation_when_redirected_then_dispatch_spacing_survives()
-> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let wire_url = Url::parse(&format!("http://{}/fixture", listener.local_addr()?))?;
    let mut transport = Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: Default::default(),
        headless: true,
        rps: 2.0,
    })?;
    transport.http_test_url = Some(wire_url);
    transport.clients.insert(
        "http://www.milesplit.com".into(),
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()?,
    );
    let gate = Arc::new((Notify::new(), Notify::new()));
    transport.http_preparation_gate = Some(Arc::clone(&gate));
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "http://www.milesplit.com/athletes/fixture".into(),
        PageContext::default(),
    );
    let controlled_preparation = async {
        gate.0.notified().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::time::resume();
        let released = Instant::now();
        gate.1.notify_one();
        released
    };
    let completed = timeout(Duration::from_secs(6), async {
        tokio::join!(
            serve(listener),
            transport.fetch(&target),
            controlled_preparation
        )
    })
    .await;
    let closed = transport.close().await;
    let (arrivals, capture, released) =
        completed.context("controlled HTTP pacing fixture timed out")?;
    closed?;
    let arrivals = arrivals?;
    let capture = capture?;
    assert_eq!(capture.status, 200);
    assert_eq!(capture.body, b"ok");
    assert_eq!(arrivals.len(), 2);
    let first = *arrivals.first().context("first request missing")?;
    let second = *arrivals.get(1).context("second request missing")?;
    assert!(first >= released);
    let upper_bound_on_wire_gap = second.duration_since(released);
    eprintln!(
        "controlled preparation held 1s; first receipt after release={:?}; second receipt after release={upper_bound_on_wire_gap:?}; observed receipt gap={:?}",
        first.duration_since(released),
        second.duration_since(first),
    );
    assert!(
        upper_bound_on_wire_gap >= Duration::from_millis(490),
        "two actual request writes occurred within {upper_bound_on_wire_gap:?} after the preparation gate opened; no first request could have been written before release"
    );
    Ok(())
}
