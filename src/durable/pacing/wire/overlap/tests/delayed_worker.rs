use super::*;

#[tokio::test]
async fn independent_workers_do_not_bunch_after_a_delayed_local_start() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let url = format!("http://{}", listener.local_addr()?);
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .build()?;
            let first = PersistentPacing::new(pool.clone())
                .with_overlapped_starts()
                .with_wire_context(&"1".repeat(64), &"b".repeat(64))?;
            let second = PersistentPacing::new(pool.clone())
                .with_overlapped_starts()
                .with_wire_context(&"2".repeat(64), &"b".repeat(64))?;
            let interval = Duration::from_micros(333_334);
            let mut held = first.starts.admit(Source::MileSplit, Duration::from_millis(1)).await?;
            *held = Some(Instant::now());
            let starts = std::sync::Mutex::new(Vec::new());
            let first_send = first.send(request()?, interval, async {
                starts.lock().expect("first start observer").push(Instant::now());
                Ok(client.get(&url).send().await?)
            });
            tokio::pin!(first_send);
            let observer = async {
                for _ in 0..100 {
                    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_wire_requests WHERE state='intent'")
                        .fetch_one(pool).await?;
                    if count == 1 {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                anyhow::bail!("first worker never committed its intent")
            };
            let calls = async {
                tokio::select! {
                    result = observer => result?,
                    result = &mut first_send => anyhow::bail!("held worker completed before release: {result:?}"),
                }
                let second_send = second.send(request()?, interval, async {
                    starts.lock().expect("second start observer").push(Instant::now());
                    Ok(client.get(&url).send().await?)
                });
                tokio::pin!(second_send);
                let completed = tokio::time::timeout(Duration::from_secs(1), &mut second_send).await;
                drop(held);
                match completed {
                    Ok(response) => {
                        assert_eq!(response?.status().as_u16(), 200);
                        assert_eq!(first_send.await?.status().as_u16(), 200);
                    }
                    Err(_) => {
                        let (first_result, second_result) = tokio::join!(&mut first_send, &mut second_send);
                        assert_eq!(first_result?.status().as_u16(), 200);
                        assert_eq!(second_result?.status().as_u16(), 200);
                    }
                }
                Ok::<_, anyhow::Error>(())
            };
            let (arrivals, sent) = tokio::time::timeout(Duration::from_secs(6), async {
                tokio::join!(serve(listener, 2, Duration::ZERO), calls)
            }).await?;
            sent?;
            assert_eq!(arrivals?.len(), 2);
            let starts = starts.lock().expect("start observer").clone();
            assert_eq!(starts.len(), 2);
            let gap = starts[1].duration_since(starts[0]);
            eprintln!("independent worker actual operation start gap: {gap:?}; required: {interval:?}");
            assert!(gap >= interval, "a delayed worker bunched starts after another worker: {gap:?}");
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn failed_start_completion_retains_observed_denial_and_prevents_another_wire() -> Result<()> {
    use crate::durable::pacing::test_support::{FixtureKind, WireFixture};
    with_database(|pool| Box::pin(async move {
        let fixture = WireFixture::start(FixtureKind::Page { status: 403, body: "denied" }).await?;
        let client = reqwest::Client::builder().no_proxy().retry(reqwest::retry::never()).build()?;
        let response = client.get(fixture.url.clone()).send().await?;
        assert_eq!(response.status().as_u16(), 403);
        sqlx::raw_sql("CREATE FUNCTION reject_start_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.lease_token IS NOT NULL AND NEW.lease_token IS NULL THEN RAISE EXCEPTION USING ERRCODE='55P03', MESSAGE='controlled start completion failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_start_completion BEFORE UPDATE ON durable_wire_pacing FOR EACH ROW EXECUTE FUNCTION reject_start_completion();")
            .execute(pool).await?;
        let gate = PersistentPacing::new(pool.clone()).with_overlapped_starts().with_wire_context(&"3".repeat(64), &"b".repeat(64))?;
        let error = gate.send(request()?, Duration::from_millis(1), async { Ok(response) }).await.expect_err("controlled completion must fail");
        let denial = error.downcast_ref::<crate::transport::AccessStopCaptureError>().context("observed denial remains primary")?;
        assert_eq!(denial.status, 403);
        assert!(format!("{error:#}").contains("controlled start completion failure"));
        let saved: (i32, bool, String) = sqlx::query_as("SELECT status,execute_started_at IS NOT NULL,state FROM durable_wire_requests").fetch_one(pool).await?;
        assert_eq!(saved, (403, true, "headers".into()));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_pacing WHERE lease_token IS NOT NULL").fetch_one(pool).await?, 2);
        let restarted = gate.restart_wire_sequence()?;
        let mut called = false;
        let denied = restarted.send(request()?, Duration::from_millis(1), async {
            called = true;
            anyhow::bail!("denial must prevent another operation")
        }).await.expect_err("replay remains denied");
        assert!(!called);
        assert!(denied.is::<crate::transport::AccessStop>());
        assert_eq!(fixture.finish().await?.len(), 1);
        Ok(())
    })).await
}

#[tokio::test(start_paused = true)]
async fn observed_access_stop_survives_start_timeout_and_failed_wire_recording() -> Result<()> {
    for status in [Some(403), Some(429), Some(200), None] {
        let elapsed =
            tokio::time::timeout(Duration::from_millis(1), futures::future::pending::<()>())
                .await
                .expect_err("controlled completion deadline");
        let error = super::super::finish_observed(
            Err(anyhow::Error::from(elapsed)
                .context("durable overlapped HTTP ownership deadline reached")),
            Err(sqlx::Error::PoolClosed.into()),
            status,
            "https://wi.milesplit.com/athletes/fixture",
        )
        .expect_err("timeout and recording failure remain failures");
        let failure = crate::transport::failure(&error);
        assert!(
            failure.detail.contains("ownership deadline reached"),
            "{}",
            failure.detail
        );
        assert!(
            failure.detail.contains("wire result recording failed"),
            "{}",
            failure.detail
        );
        assert!(
            failure.detail.contains("deadline has elapsed"),
            "{}",
            failure.detail
        );
        if matches!(status, Some(403 | 429)) {
            assert!(failure.is_blocked());
            assert!(!failure.is_retryable());
            assert_eq!(failure.stopped_origin(), Some("https://wi.milesplit.com"));
            assert_eq!(
                Some(
                    error
                        .downcast_ref::<crate::transport::AccessStopCaptureError>()
                        .context("observed access status")?
                        .status
                ),
                status
            );
        } else {
            assert!(!failure.is_blocked());
            assert!(failure.is_retryable());
            assert!(failure.stopped_origin().is_none());
        }
    }
    Ok(())
}
