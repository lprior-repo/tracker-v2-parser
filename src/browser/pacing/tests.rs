use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};

use super::Pacing;

#[tokio::test]
async fn given_delayed_first_send_and_headers_when_next_wire_is_admitted_then_completed_receipts_anchor_spacing()
-> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}/fixture", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut received = Vec::new();
        let mut first_headers = None;
        for ordinal in 0..2 {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = [0_u8; 4096];
            ensure!(
                socket.read(&mut bytes).await? > 0,
                "fixture request missing"
            );
            received.push(Instant::now());
            if ordinal == 0 {
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await?;
            if ordinal == 0 {
                first_headers = Some(Instant::now());
            }
        }
        Ok::<_, anyhow::Error>((
            received,
            first_headers.context("first headers were not sent")?,
        ))
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(5))
        .build()?;
    let interval = Duration::from_millis(200);
    let pacing = Pacing::new(interval);
    let scenario = tokio::time::timeout(Duration::from_secs(8), async {
        let first = pacing.admit().await?;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let response = client
            .get(&address)
            .send()
            .await
            .map_err(anyhow::Error::from);
        let response = pacing.complete_response(first, response, &address).await?;
        assert_eq!(response.status().as_u16(), 200);
        let second = pacing.admit().await?;
        let response = client
            .get(&address)
            .send()
            .await
            .map_err(anyhow::Error::from);
        let response = pacing.complete_response(second, response, &address).await?;
        assert_eq!(response.status().as_u16(), 200);
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let observed = tokio::time::timeout(Duration::from_secs(8), server).await;
    scenario??;
    let (received, first_headers) = observed???;
    assert_eq!(received.len(), 2);
    assert!(
        received[1].duration_since(first_headers)
            >= interval.saturating_sub(Duration::from_millis(10))
    );
    assert!(
        received[1].duration_since(received[0])
            >= interval.saturating_sub(Duration::from_millis(10))
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn given_abandoned_wire_lease_when_admission_restarts_then_interval_and_forty_five_second_debt_remain()
-> Result<()> {
    let interval = Duration::from_millis(500);
    let pacing = Pacing::new(interval);
    let abandoned = pacing.admit().await?;
    let start = Instant::now();
    let recovered = pacing.admit().await?;
    assert_eq!(start.elapsed(), Duration::from_millis(45_500));
    assert!(pacing.complete(abandoned).await.is_err());
    pacing.complete(recovered).await?;
    let start = Instant::now();
    let next = pacing.admit().await?;
    assert_eq!(start.elapsed(), interval);
    pacing.complete(next).await?;
    Ok(())
}

#[tokio::test]
async fn given_zero_interval_and_pending_headers_when_another_request_starts_then_wire_can_progress_concurrently()
-> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let (observed, first_observed) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let mut server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await?;
        let mut bytes = [0_u8; 4096];
        let count = first.read(&mut bytes).await?;
        ensure!(
            String::from_utf8_lossy(&bytes[..count]).starts_with("GET /hold "),
            "first request missing"
        );
        observed
            .send(())
            .map_err(|()| anyhow::anyhow!("first request observer closed"))?;
        let (mut second, _) = listener.accept().await?;
        let count = second.read(&mut bytes).await?;
        ensure!(
            String::from_utf8_lossy(&bytes[..count]).starts_with("GET /ready "),
            "concurrent request missing"
        );
        second
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await?;
        released.await.context("first headers were not released")?;
        first
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await?;
        Ok::<_, anyhow::Error>(())
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(5))
        .build()?;
    let pacing = Pacing::new(Duration::ZERO);
    let first = async {
        let lease = pacing.admit().await?;
        let response = client
            .get(format!("{address}/hold"))
            .send()
            .await
            .map_err(anyhow::Error::from);
        assert_eq!(
            pacing
                .complete_response(lease, response, &address)
                .await?
                .status()
                .as_u16(),
            200
        );
        Ok::<_, anyhow::Error>(())
    };
    let second = async {
        first_observed
            .await
            .context("first request was not observed")?;
        let lease = pacing.admit().await?;
        let response = client
            .get(format!("{address}/ready"))
            .send()
            .await
            .map_err(anyhow::Error::from);
        assert_eq!(
            pacing
                .complete_response(lease, response, &address)
                .await?
                .status()
                .as_u16(),
            200
        );
        release
            .send(())
            .map_err(|()| anyhow::anyhow!("first response release closed"))?;
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::try_join!(first, second)
    })
    .await;
    if !matches!(&result, Ok(Ok(_))) {
        server.abort();
        match server.await {
            Err(error) if error.is_cancelled() => {}
            Err(error) => return Err(error.into()),
            Ok(outcome) => outcome?,
        }
        return result
            .context("zero interval request watchdog expired")?
            .map(|_| ());
    }
    tokio::time::timeout(Duration::from_secs(5), &mut server).await???;
    pacing.stop(address, 403)?;
    assert!(pacing.admit().await.is_err());
    Ok(())
}
