use std::time::Duration;

mod admission;
mod cadence;

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
async fn given_a_crash_after_wire_when_same_attempt_replays_then_intent_prevents_another_http_call()
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
        assert_eq!(fixture.finish().await?.len(), 1);
        let evidence: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(r) FROM durable_wire_requests r").fetch_one(pool).await?;
        assert_eq!(evidence["state"], "intent");
        assert!(evidence["status"].is_null());
        assert!(!evidence.to_string().contains("wire-path-canary"));
        assert!(!evidence.to_string().contains("wire-body-canary"));
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
            assert!(failure.blocked);
            assert_eq!(
                failure.stopped_origin.as_deref(),
                Some("https://wi.milesplit.com")
            );
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
