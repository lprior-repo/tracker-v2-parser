use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use super::{MAX_CACHED_ORIGINS, SharedHttpClients};

fn addresses() -> Vec<std::net::SocketAddr> {
    vec!["8.8.8.8:80".parse().expect("public fixture address")]
}

#[tokio::test(start_paused = true)]
async fn cache_requires_public_nonempty_dns_and_exact_address_identity() -> Result<()> {
    let cache = SharedHttpClients::default();
    let url = Url::parse("http://www.milesplit.com/")?;
    cache.client(&url, &addresses())?;
    let created = cache
        .state()?
        .get(&url.origin().ascii_serialization())
        .context("entry")?
        .created;
    tokio::time::advance(Duration::from_secs(1)).await;
    cache.client(&url, &addresses())?;
    assert_eq!(cache.state()?.len(), 1);
    assert_eq!(
        cache
            .state()?
            .get(&url.origin().ascii_serialization())
            .context("entry")?
            .created,
        created
    );
    for invalid in [
        vec![],
        vec!["127.0.0.1:80".parse()?],
        vec!["8.8.8.8:80".parse()?, "10.0.0.1:80".parse()?],
    ] {
        assert!(cache.client(&url, &invalid).is_err());
    }
    assert_eq!(
        cache
            .state()?
            .get(&url.origin().ascii_serialization())
            .context("entry")?
            .created,
        created
    );
    let changed = vec!["1.1.1.1:80".parse()?];
    cache.client(&url, &changed)?;
    assert_eq!(
        cache
            .state()?
            .get(&url.origin().ascii_serialization())
            .context("entry")?
            .addresses,
        changed
    );
    assert!(
        cache
            .state()?
            .get(&url.origin().ascii_serialization())
            .context("entry")?
            .created
            > created
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cache_expires_without_reuse_extending_ttl_and_remains_bounded() -> Result<()> {
    let cache = SharedHttpClients::default();
    let first = Url::parse("http://school-0.example.edu/")?;
    for index in 0..MAX_CACHED_ORIGINS {
        cache.client(
            &Url::parse(&format!("http://school-{index}.example.edu/"))?,
            &addresses(),
        )?;
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    assert_eq!(cache.state()?.len(), MAX_CACHED_ORIGINS);
    cache.client(&Url::parse("http://extra.example.edu/")?, &addresses())?;
    assert_eq!(cache.state()?.len(), MAX_CACHED_ORIGINS);
    assert!(
        !cache
            .state()?
            .contains_key(&first.origin().ascii_serialization())
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    cache.client(&first, &addresses())?;
    assert_eq!(cache.state()?.len(), 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn equivalent_dns_order_and_duplicates_reuse_one_entry() -> Result<()> {
    let cache = SharedHttpClients::default();
    let url = Url::parse("https://www.milesplit.com/")?;
    let first = vec!["8.8.8.8:443".parse()?, "1.1.1.1:443".parse()?];
    cache.client(&url, &first)?;
    let created = cache
        .state()?
        .get(&url.origin().ascii_serialization())
        .context("entry")?
        .created;
    tokio::time::advance(Duration::from_millis(1)).await;
    cache.client(&url, &[first[1], first[0], first[1]])?;
    assert_eq!(
        cache
            .state()?
            .get(&url.origin().ascii_serialization())
            .context("entry")?
            .created,
        created
    );
    assert_eq!(
        cache
            .state()?
            .get(&url.origin().ascii_serialization())
            .context("entry")?
            .addresses
            .len(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn cached_pool_survives_transport_close_and_reuses_one_tcp_connection() -> Result<()> {
    let cache = SharedHttpClients::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let origin = Url::parse("http://www.milesplit.com/")?;
    let server = async {
        let (mut socket, _) = listener.accept().await?;
        for _ in 0..2 {
            let mut request = Vec::new();
            for _ in 0..2048 {
                request.push(socket.read_u8().await?);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            anyhow::ensure!(request.ends_with(b"\r\n\r\n"), "fixture header limit");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                )
                .await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let requests = async {
        let transport = crate::transport::Transport::new(crate::transport::Options {
            mode: crate::transport::Mode::Http,
            driver_url: None,
            profile: Default::default(),
            headless: true,
            rps: 3.0,
        })?
        .with_shared_http_clients(cache.clone());
        let client = cache.client(&origin, &addresses())?;
        assert_eq!(
            client
                .get(format!("http://{address}/first"))
                .send()
                .await?
                .text()
                .await?,
            "ok"
        );
        drop(client);
        transport.close().await?;
        let client = cache.client(&origin, &addresses())?;
        assert_eq!(
            client
                .get(format!("http://{address}/second"))
                .send()
                .await?
                .text()
                .await?,
            "ok"
        );
        Ok::<_, anyhow::Error>(())
    };
    let (server, requests) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, requests)
    })
    .await?;
    server?;
    requests?;
    Ok(())
}

#[tokio::test]
async fn large_public_dns_sets_bypass_cache_without_expanding_retained_state() -> Result<()> {
    let cache = SharedHttpClients::default();
    let addresses = (1..=33)
        .map(|last| std::net::SocketAddr::from(([8, 8, 8, last], 80)))
        .collect::<Vec<_>>();
    cache.client(&Url::parse("http://www.milesplit.com/")?, &addresses)?;
    assert!(cache.state()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn reused_cached_client_does_not_bypass_durable_saved_stop() -> Result<()> {
    use crate::durable::pacing::{
        PersistentPacing, WireRequest,
        test_support::{FixtureKind, WireFixture, with_database},
    };
    with_database(|pool| {
        Box::pin(async move {
            let cache = SharedHttpClients::default();
            let origin = Url::parse("https://wi.milesplit.com/")?;
            let addresses = vec!["8.8.8.8:443".parse()?];
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 403,
                body: "denied",
            })
            .await?;
            let request = || {
                WireRequest::new(
                    crate::model::Source::MileSplit,
                    "https://wi.milesplit.com/athletes/fixture",
                    "GET",
                    b"",
                )
            };
            let first = PersistentPacing::new(pool.clone())
                .with_wire_context(&"1".repeat(64), &"2".repeat(64))?;
            let client = cache.client(&origin, &addresses)?;
            let response = first
                .send(request()?, Duration::from_millis(333_334), async {
                    Ok(client.get(fixture.url.clone()).send().await?)
                })
                .await?;
            assert_eq!(response.status().as_u16(), 403);
            let second = PersistentPacing::new(pool.clone())
                .with_wire_context(&"3".repeat(64), &"4".repeat(64))?;
            let client = cache.client(&origin, &addresses)?;
            let result = second
                .send(request()?, Duration::from_millis(333_334), async {
                    Ok(client.get(fixture.url.clone()).send().await?)
                })
                .await;
            assert!(
                result
                    .expect_err("saved stop must prevent reuse from sending")
                    .is::<crate::transport::AccessStop>()
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                1
            );
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}
