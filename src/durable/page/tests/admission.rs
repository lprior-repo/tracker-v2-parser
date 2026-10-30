use std::time::Duration;

use anyhow::Result;

use super::{page, scope, target, transport};
use crate::{
    database::Database,
    durable::{
        AcquisitionBusy,
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{FailureClass, FetchDecision, PageOutcome, ReservationDecision},
    },
};

#[tokio::test]
async fn real_fetch_admission_contention_is_persisted_as_deferred_and_resumes_without_a_duplicate_get()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database = Database::durable_test_database(pool.clone());
        let root = tempfile::tempdir()?;
        let request = page::bind_target(&database, target(), &scope()?).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let ReservationDecision::Reserved { reservation } = page::reserve(&database, &request, now).await? else { anyhow::bail!("initial reservation") };
        sqlx::query("INSERT INTO durable_wire_pacing(pacing_key,lease_token,next_allowed_at) SELECT key,repeat('a',32),clock_timestamp()+INTERVAL '1 hour' FROM UNNEST($1::text[]) key")
            .bind(vec!["provider:milesplit", "origin:https://oh.milesplit.com"])
            .execute(pool).await?;
        let fixture = WireFixture::start(FixtureKind::Page { status:200,body:"<html>reachable source</html>" }).await?;
        let mut connection = transport(root.path(), &fixture, &database)?.with_pacing_test_polls(1)?;
        let FetchDecision::Settled { outcome: PageOutcome::Deferred { retry_at_ms, cycle } } = page::fetch(&database, root.path(), &reservation, &mut connection, now).await? else { anyhow::bail!("contention must defer") };
        assert_eq!(cycle, 1);
        assert!(retry_at_ms >= now + 1000);
        let state: (String, i64, serde_json::Value) = sqlx::query_as("SELECT state,retry_at_ms,outcome FROM durable_page_operations WHERE operation_id=$1")
            .bind(&request.binding.operation_id).fetch_one(pool).await?;
        assert_eq!(state, ("deferred".into(), retry_at_ms, serde_json::json!({"state":"deferred","retry_at_ms":retry_at_ms,"cycle":1})));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 0);
        assert!(matches!(page::reserve(&database, &request, retry_at_ms - 1).await?, ReservationDecision::Settled { outcome: PageOutcome::Deferred { .. } }));
        connection.close().await?;
        sqlx::query("UPDATE durable_wire_pacing SET lease_token=NULL,next_allowed_at=clock_timestamp()")
            .execute(pool).await?;
        let ReservationDecision::Reserved { reservation: retry } = page::reserve(&database, &request, retry_at_ms).await? else { anyhow::bail!("due retry reservation") };
        assert_eq!((retry.cycle, retry.ordinal, retry.total_attempts), (1,2,2));
        let mut recovered = transport(root.path(), &fixture, &database)?;
        let FetchDecision::Captured { capture } = page::fetch(&database, root.path(), &retry, &mut recovered, retry_at_ms).await? else { anyhow::bail!("capture after capacity recovers") };
        let FetchDecision::Captured { capture: replayed } = page::fetch(&database, root.path(), &retry, &mut recovered, retry_at_ms).await? else { anyhow::bail!("retained capture replay") };
        assert_eq!(capture, replayed);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(pool).await?, 1);
        recovered.close().await?;
        assert_eq!(fixture.finish().await?.len(), 1);
        Ok(())
    })).await
}

#[tokio::test(start_paused = true)]
async fn elapsed_admission_retains_retry_metadata_at_transport_boundary() -> Result<()> {
    let elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
        .await
        .expect_err("pending operation must time out");
    let error = anyhow::Error::from(elapsed)
        .context("persistent wire admission exceeded bounded wait budget");
    let failure = crate::transport::failure(&error);
    assert!(failure.retryable);
    assert!(!failure.blocked);
    assert_eq!(page::classify(&failure), FailureClass::Transient);
    assert!(!failure.detail.contains("timeout"));
    assert!(!failure.detail.contains("timed out"));
    Ok(())
}

#[test]
fn retry_metadata_never_overrides_access_stops_or_captured_invalid_results() {
    let busy = anyhow::Error::from(AcquisitionBusy).context("bounded admission contention");
    let mut failure = crate::transport::failure(&busy);
    assert!(failure.retryable);
    assert_eq!(page::classify(&failure), FailureClass::Transient);
    failure.capture = Some(crate::model::Capture {
        url: target().url,
        status: 200,
        content_type: "text/html".into(),
        body: Vec::new(),
    });
    assert_eq!(page::classify(&failure), FailureClass::Rejected);
    let stop = busy.context(crate::transport::AccessStop {
        origin: "https://oh.milesplit.com".into(),
        status: Some(403),
    });
    let failure = crate::transport::failure(&stop);
    assert!(failure.retryable);
    assert!(failure.blocked);
    assert_eq!(page::classify(&failure), FailureClass::Blocked);
    let invalid = anyhow::anyhow!("persistent wire admission exceeded bounded contention budget");
    let failure = crate::transport::failure(&invalid);
    assert!(!failure.retryable);
    assert_eq!(page::classify(&failure), FailureClass::Rejected);
}
