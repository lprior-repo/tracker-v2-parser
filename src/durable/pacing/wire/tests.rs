use std::time::Duration;

mod admission;
mod cadence;
mod delayed_intent;

use anyhow::{Context, Result};

use super::{PersistentPacing, WireRequest};
use crate::{
    durable::pacing::test_support::{FixtureKind, WireFixture, with_database},
    model::Source,
};

fn request() -> Result<WireRequest> {
    WireRequest::new(
        Source::MileSplit,
        "https://wi.milesplit.com/athletes/fixture?public_query=wire-path-canary",
        "GET",
        b"wire-body-canary",
    )
}

#[tokio::test]
async fn given_an_admitted_exclusive_wire_when_http_times_out_then_intent_and_lease_settle()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let gate = PersistentPacing::new(pool.clone())
                .with_wire_context(&"1".repeat(64), &"2".repeat(64))?;
            let mut started = false;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            let error = gate
                .send_with_deadline(
                    request()?,
                    Duration::from_millis(500),
                    async {
                        started = true;
                        futures::future::pending().await
                    },
                    Some(deadline),
                )
                .await
                .expect_err("pending HTTP operation must time out");

            assert!(started);
            assert!(format!("{error:#}").contains("source HTTP request timed out"));
            let wire: (String, Option<i32>, Option<String>) =
                sqlx::query_as("SELECT state, status, error_class FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?;
            assert_eq!(wire, ("failed".into(), None, Some("transport".into())));
            let leases: (i64, i64) =
                sqlx::query_as("SELECT count(*), count(lease_token) FROM durable_wire_pacing")
                    .fetch_one(pool)
                    .await?;
            assert_eq!(leases, (2, 0));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn exclusive_wire_retains_an_observed_429_when_receipt_settlement_crosses_the_network_deadline()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let fixture = WireFixture::start(FixtureKind::Page { status: 429, body: "denied" }).await?;
        let client = reqwest::Client::builder().no_proxy().build()?;
        sqlx::raw_sql(
            "CREATE FUNCTION hold_denied_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_advisory_xact_lock(hashtext(current_schema()), 43); \
             RAISE EXCEPTION 'controlled denied receipt failure'; END $$; \
             CREATE TRIGGER hold_denied_receipt BEFORE UPDATE ON durable_wire_requests \
             FOR EACH ROW WHEN (NEW.status=429) EXECUTE FUNCTION hold_denied_receipt();",
        ).execute(pool).await?;
        let mut blocker = pool.begin().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext(current_schema()), 43)")
            .execute(&mut *blocker).await?;
        let gate = PersistentPacing::new(pool.clone())
            .with_wire_context(&"1".repeat(64), &"2".repeat(64))?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let send = gate.send_with_deadline(request()?, Duration::from_millis(1), async {
            Ok(client.get(fixture.url.clone()).send().await?)
        }, Some(deadline));
        let release = async {
            tokio::time::timeout_at(deadline, async {
                loop {
                    let blocked: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
                    ).bind(pid).fetch_one(pool).await?;
                    if blocked {
                        return Ok::<_, sqlx::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.context("receipt write did not start before the deadline")??;
            tokio::time::sleep_until(deadline + Duration::from_millis(20)).await;
            blocker.commit().await?;
            Ok::<_, anyhow::Error>(())
        };
        let (result, released) = tokio::join!(send, release);
        released?;
        let failure = result.expect_err("receipt settlement must fail");
        let observed = failure.downcast_ref::<crate::transport::AccessStopCaptureError>()
            .context("retained access denial")?;
        assert_eq!(observed.status, 429);
        assert!(format!("{failure:#}").contains("controlled denied receipt failure"));
        assert_eq!(sqlx::query_as::<_, (i64, i64)>(
            "SELECT count(*), count(lease_token) FROM durable_wire_pacing",
        ).fetch_one(pool).await?, (2, 0));
        assert_eq!(fixture.finish().await?.len(), 1);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_a_cancelled_wire_when_its_lease_expires_then_only_a_new_attempt_can_send()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let fixture = WireFixture::start(FixtureKind::Page { status: 200, body: "ok" }).await?;
        let gate = PersistentPacing::new(pool.clone()).with_wire_context(&"1".repeat(64), &"2".repeat(64))?;
        let client = reqwest::Client::builder().no_proxy().build()?;
        let (entered, received) = tokio::sync::oneshot::channel();
        {
            let operation = gate.send(request()?, Duration::from_millis(500), async {
                let response = client.get(fixture.url.clone()).body("wire-body-canary").send().await?;
                entered.send(()).expect("wire observer");
                futures::future::pending::<()>().await;
                Ok(response)
            });
            tokio::pin!(operation);
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    entered = received => entered.context("wire observer closed"),
                    result = &mut operation => anyhow::bail!("wire effect ended before crash: {result:?}"),
                }
            }).await??;
        }
        let replay = gate.restart_wire_sequence()?;
        let repeated = tokio::time::timeout(Duration::from_secs(1), replay.send(request()?, Duration::from_millis(500), async {
            Ok(client.get(fixture.url.clone()).body("wire-body-canary").send().await?)
        })).await.context("existing wire intent should reject before waiting for crash reservation")?;
        assert!(format!("{:#}", repeated.expect_err("same-attempt replay must not send")).contains("network interruption"));
        let evidence: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(r) FROM durable_wire_requests r").fetch_one(pool).await?;
        assert_eq!(evidence["state"], "intent");
        assert!(evidence["status"].is_null());
        assert!(!evidence.to_string().contains("wire-path-canary"));
        assert!(!evidence.to_string().contains("wire-body-canary"));
        assert_eq!(sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM durable_wire_pacing WHERE lease_token IS NOT NULL \
             AND next_allowed_at > clock_timestamp() \
             AND next_allowed_at - updated_at = INTERVAL '45.5 seconds'",
        ).fetch_one(pool).await?, 2);
        let expired = sqlx::query(
            "UPDATE durable_wire_pacing SET next_allowed_at=clock_timestamp() WHERE lease_token IS NOT NULL",
        ).execute(pool).await?;
        assert_eq!(expired.rows_affected(), 2);
        let next = PersistentPacing::new(pool.clone()).with_wire_context(&"1".repeat(64), &"3".repeat(64))?;
        let response = tokio::time::timeout(Duration::from_secs(2), next.send(
            request()?, Duration::from_millis(500), async {
                Ok(client.get(fixture.url.clone()).body("wire-body-canary").send().await?)
            },
        )).await??;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let original: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(r) FROM durable_wire_requests r WHERE attempt_id=$1",
        ).bind("2".repeat(64)).fetch_one(pool).await?;
        assert_eq!(original, evidence);
        assert_eq!(sqlx::query_as::<_, (String, i32)>(
            "SELECT state, status FROM durable_wire_requests WHERE attempt_id=$1",
        ).bind("3".repeat(64)).fetch_one(pool).await?, ("headers".into(), 200));
        assert_eq!(fixture.finish().await?.len(), 2);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_durable_403_headers_and_a_pacing_failure_when_replayed_then_access_stop_is_preserved()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let fixture = WireFixture::start(FixtureKind::Page {
                status: 403,
                body: "denied",
            })
            .await?;
            let gate = PersistentPacing::new(pool.clone())
                .with_wire_context(&"1".repeat(64), &"2".repeat(64))?;
            let client = reqwest::Client::builder().no_proxy().build()?;
            let result = gate
                .send(request()?, Duration::from_millis(500), async {
                    let response = client.get(fixture.url.clone()).send().await?;
                    sqlx::query("DROP TABLE durable_wire_pacing")
                        .execute(pool)
                        .await?;
                    Ok(response)
                })
                .await
                .expect_err("pacing completion failure");
            let failure = crate::transport::failure(&result);
            assert!(failure.is_blocked());
            assert_eq!(failure.stopped_origin(), Some("https://wi.milesplit.com"));
            assert!(failure.detail.contains("403"));
            let replay = gate.with_wire_context(&"3".repeat(64), &"4".repeat(64))?;
            let repeated = replay
                .send(request()?, Duration::from_millis(500), async {
                    Ok(client.get(fixture.url.clone()).send().await?)
                })
                .await
                .expect_err("saved403 stops before the missing pace table or wire");
            assert!(repeated.is::<crate::transport::AccessStop>());
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}
