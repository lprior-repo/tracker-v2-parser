mod measurement;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use futures::{StreamExt, TryStreamExt, future::join_all, stream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};

use super::{PersistentPacing, WireRequest};
use crate::{
    durable::pacing::{Admission, Spacing, WireScope, test_support::with_database},
    model::Source,
};

fn request() -> Result<WireRequest> {
    WireRequest::new(
        Source::MileSplit,
        "https://wi.milesplit.com/athletes/fixture",
        "GET",
        b"",
    )
}

async fn serve(listener: TcpListener, count: usize, delay: Duration) -> Result<Vec<Instant>> {
    anyhow::ensure!(count <= 3, "fixture exceeds bounded concurrency");
    let listener = Arc::new(listener);
    stream::iter(0..count)
        .map(|_| {
            let listener = listener.clone();
            async move {
                let (mut socket, _) = listener.accept().await?;
                let mut bytes = [0; 1024];
                anyhow::ensure!(socket.read(&mut bytes).await? > 0, "empty request");
                let arrived = Instant::now();
                tokio::time::sleep(delay).await;
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await?;
                Ok::<_, anyhow::Error>(arrived)
            }
        })
        .buffer_unordered(3)
        .try_collect()
        .await
}

#[tokio::test]
async fn independent_start_admission_overlaps_slow_headers_without_exceeding_three_rps()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<std::net::SocketAddr>()?).await?;
        let url = format!("http://{}", listener.local_addr()?);
        let client = reqwest::Client::builder().no_proxy().retry(reqwest::retry::never()).build()?;
        let gate = PersistentPacing::new(pool.clone()).with_overlapped_starts();
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls = join_all((1..=3).map(|index| {
            let client = &client;
            let url = &url;
            let gate = gate.clone();
            let executed = executed.clone();
            async move {
                gate
                    .with_wire_context(&format!("{index:064x}"), &"b".repeat(64))?
                    .send(request()?, Duration::from_micros(333_334), async {
                        executed.lock().expect("execute timestamps").push(Instant::now());
                        Ok(client.get(url).send().await?)
                    }).await
            }
        }));
        let (arrivals, responses) = tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(serve(listener, 3, Duration::from_millis(900)), calls)
        }).await?;
        for response in responses { assert_eq!(response?.status().as_u16(), 200); }
        let mut arrivals = arrivals?;
        arrivals.sort_unstable();
        let span = arrivals.last().context("last")?.duration_since(*arrivals.first().context("first")?);
        assert!(span < Duration::from_millis(850), "headers remained exclusive: {span:?}");
        let starts: Vec<(i64,)> = sqlx::query_as("SELECT round(EXTRACT(EPOCH FROM (started_at-LAG(started_at) OVER(ORDER BY started_at)))*1000000)::bigint FROM durable_wire_requests ORDER BY started_at OFFSET 1")
            .fetch_all(pool).await?;
        assert_eq!(starts.len(), 2);
        assert!(starts.iter().all(|(micros,)| *micros >= 333_334), "early start: {starts:?}");
        let executes = executed.lock().expect("execute timestamps").clone();
        assert_eq!(executes.len(),3);
        assert!(executes.windows(2).all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_micros(333_334)), "client execute starts bunched: {executes:?}");
        let measured: Vec<(i64,)> = sqlx::query_as("SELECT round(EXTRACT(EPOCH FROM (execute_started_at-LAG(execute_started_at) OVER(ORDER BY execute_started_at)))*1000000)::bigint FROM durable_wire_requests WHERE execute_started_at IS NOT NULL ORDER BY execute_started_at OFFSET 1")
            .fetch_all(pool).await?;
        assert_eq!(measured.len(), 2);
        assert!(measured.iter().all(|(micros,)| *micros >= 333_334), "persisted execute starts bunched: {measured:?}");
        eprintln!("v2 admission gaps_us={starts:?}; execute gaps_us={:?}; server arrival gaps_us={:?}", executes.windows(2).map(|pair|pair[1].duration_since(pair[0]).as_micros()).collect::<Vec<_>>(),arrivals.windows(2).map(|pair|pair[1].duration_since(pair[0]).as_micros()).collect::<Vec<_>>());
        Ok(())
    })).await
}

#[tokio::test]
async fn late_wire_completion_cannot_rewind_a_newer_provider_start_timestamp() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let first = PersistentPacing::new(pool.clone())
                .with_overlapped_starts()
                .with_wire_context(&"1".repeat(64), &"b".repeat(64))?;
            let second = PersistentPacing::new(pool.clone())
                .with_overlapped_starts()
                .with_wire_context(&"2".repeat(64), &"b".repeat(64))?;
            let request = request()?;
            let first_receipt = first
                .wire_context
                .as_ref()
                .context("first context")?
                .next(&request)?;
            let second_receipt = second
                .wire_context
                .as_ref()
                .context("second context")?
                .next(&request)?;
            assert!(
                first
                    .reserve_start(&request, &first_receipt, Duration::from_millis(1))
                    .await?
                    .is_none()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert!(
                second
                    .reserve_start(&request, &second_receipt, Duration::from_millis(333_334))
                    .await?
                    .is_none()
            );
            let before: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY pacing_key) FROM durable_wire_pacing p",
            )
            .fetch_one(pool)
            .await?;
            first.complete_wire(&first_receipt, Some(200)).await?;
            let after: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY pacing_key) FROM durable_wire_pacing p",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(after, before);
            assert!(
                first
                    .complete_wire(&first_receipt, Some(200))
                    .await
                    .is_err()
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM durable_wire_requests WHERE state='intent'"
                )
                .fetch_one(pool)
                .await?,
                1
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn old_exclusive_lease_and_saved_stop_remain_authoritative_for_overlap_mode() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let old = PersistentPacing::new(pool.clone());
        let scope = WireScope::parse(Source::MileSplit, "https://wi.milesplit.com/", Duration::from_millis(1), Spacing::CompletedHeaders)?;
        let token = match old.reserve(&scope).await? { Admission::Reserved(token) => token, _ => anyhow::bail!("fresh legacy reservation unexpectedly waits") };
        let overlap = PersistentPacing::new(pool.clone()).with_overlapped_starts().with_wire_context(&"1".repeat(64), &"b".repeat(64))?;
        let request = request()?;
        let receipt = overlap.wire_context.as_ref().context("context")?.next(&request)?;
        assert!(overlap.reserve_start(&request, &receipt, Duration::from_millis(333_334)).await?.is_some());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 0);
        old.complete(&scope, &token).await?;
        sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',repeat('c',64),'https://school.example.org/',jsonb_build_object('kind','contacts','access_stop_url','https://wi.milesplit.com'),'blocked',1,'retained public access stop')").execute(pool).await?;
        assert!(overlap.reserve_start(&request, &receipt, Duration::from_millis(333_334)).await.expect_err("saved stop prevents intent").is::<crate::transport::AccessStop>());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 0);
        Ok(())
    })).await
}

#[tokio::test]
async fn cancellation_preserves_immutable_intent_and_cannot_repeat_that_attempt() -> Result<()> {
    with_database(|pool| Box::pin(async move {
        let gate = PersistentPacing::new(pool.clone()).with_overlapped_starts().with_wire_context(&"1".repeat(64), &"b".repeat(64))?;
        let (entered, received) = tokio::sync::oneshot::channel();
        {
            let send = gate.send(request()?, Duration::from_micros(333_334), async {
                entered.send(()).expect("admitted observer");
                futures::future::pending::<Result<reqwest::Response>>().await
            });
            tokio::pin!(send);
            tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! { seen = received => seen.context("observer"), result = &mut send => anyhow::bail!("wire ended before cancellation: {result:?}") }
            }).await??;
        }
        let replay = gate.restart_wire_sequence()?;
        let mut called = false;
        let error = replay.send(request()?, Duration::from_micros(333_334), async { called=true; anyhow::bail!("replay must stay prewire") }).await.expect_err("same-attempt replay denied");
        assert!(!called);
        assert!(format!("{error:#}").contains("network interruption"));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests WHERE state='intent'").fetch_one(pool).await?,1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests WHERE execute_started_at IS NULL").fetch_one(pool).await?,1);
        Ok(())
    })).await
}

#[tokio::test]
async fn three_pending_intents_keep_capacity_debt_without_advancing_a_fourth_start() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let request = request()?;
            for index in 1..=3 {
                let gate = PersistentPacing::new(pool.clone())
                    .with_overlapped_starts()
                    .with_wire_context(&format!("{index:064x}"), &"b".repeat(64))?;
                let receipt = gate
                    .wire_context
                    .as_ref()
                    .context("context")?
                    .next(&request)?;
                assert!(
                    gate.reserve_start(&request, &receipt, Duration::from_millis(1))
                        .await?
                        .is_none()
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let fourth = PersistentPacing::new(pool.clone())
                .with_overlapped_starts()
                .with_wire_context(&"4".repeat(64), &"b".repeat(64))?;
            let receipt = fourth
                .wire_context
                .as_ref()
                .context("fourth context")?
                .next(&request)?;
            let before: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY pacing_key) FROM durable_wire_pacing p",
            )
            .fetch_one(pool)
            .await?;
            assert!(
                fourth
                    .reserve_start(&request, &receipt, Duration::from_millis(1))
                    .await?
                    .is_some()
            );
            let after: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(p) ORDER BY pacing_key) FROM durable_wire_pacing p",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(after, before);
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM durable_wire_requests WHERE state='intent'"
                )
                .fetch_one(pool)
                .await?,
                3
            );
            Ok(())
        })
    })
    .await
}
