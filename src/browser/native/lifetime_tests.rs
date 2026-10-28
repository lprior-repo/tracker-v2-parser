use super::NativeSink;
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};

#[tokio::test]
async fn given_many_empty_acquisitions_when_checkpointed_then_native_sink_remains_available()
-> Result<()> {
    let sink = NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let status = sink.status();
    let result = timeout(Duration::from_secs(20), async {
        for _ in 0..2049 {
            assert_eq!(status.begin(Vec::new()).await?, 0);
            assert_eq!(status.checkpoint().await?, 0);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("bounded native checkpoint fixture deadline")?;
    let closed = sink.finish().await;
    result.and(closed)
}

#[tokio::test]
async fn given_one_probe_per_acquisition_when_total_exceeds_512_then_native_sink_remains_available()
-> Result<()> {
    let sink = NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let status = sink.status();
    let address = sink.address()?;
    let result = timeout(Duration::from_secs(20), async {
        for _ in 0..513 {
            assert_eq!(status.begin(Vec::new()).await?, 0);
            drop(TcpStream::connect(address).await?);
            assert_eq!(status.checkpoint().await?, 0);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("bounded native probe fixture deadline")?;
    let closed = sink.finish().await;
    result.and(closed)
}

#[tokio::test]
async fn given_one_acquisition_when_command_budget_is_exhausted_then_native_sink_stops()
-> Result<()> {
    let sink = NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let status = sink.status();
    status.begin(Vec::new()).await?;
    let mut completed = 0;
    for _ in 0..4097 {
        if status.checkpoint().await.is_err() {
            break;
        }
        completed += 1;
    }
    let closed = sink.finish().await;
    assert_eq!(completed, 4096);
    assert!(closed.is_err());
    Ok(())
}

#[tokio::test]
async fn given_one_acquisition_when_connection_budget_is_exhausted_then_native_sink_stops()
-> Result<()> {
    let sink = NativeSink::start(TcpListener::bind("127.0.0.1:0").await?)?;
    let status = sink.status();
    let address = sink.address()?;
    status.begin(Vec::new()).await?;
    let mut completed = 0;
    for _ in 0..513 {
        drop(TcpStream::connect(address).await?);
        if status.checkpoint().await.is_err() {
            break;
        }
        completed += 1;
    }
    let closed = sink.finish().await;
    assert_eq!(completed, 512);
    assert_eq!(status.counts().0, 512);
    assert!(closed.is_err());
    Ok(())
}
