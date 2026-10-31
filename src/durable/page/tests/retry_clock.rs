use std::time::Duration;

use anyhow::{Context, Result};

use super::{page, scope, target, transport};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::{FixtureKind, WireFixture, with_database},
        page::{FailureClass, FailureRecord, FetchDecision, PageOutcome, ReservationDecision},
    },
};

#[tokio::test]
async fn slow_http_failure_retains_full_first_retry_interval_after_request_completion() -> Result<()>
{
    delayed_failure(1).await
}

#[tokio::test]
async fn slow_third_http_failure_exhausts_the_total_budget_after_request_completion() -> Result<()>
{
    delayed_failure(3).await
}

async fn delayed_failure(ordinal: i32) -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let root = tempfile::tempdir()?;
            let scope = scope()?;
            let request = page::bind_target(&database, target(), &scope).await?;
            let mut now = chrono::Utc::now().timestamp_millis();
            let mut selected = None;
            for index in 1..=ordinal {
                let ReservationDecision::Reserved { reservation } =
                    page::reserve(&database, &request, now).await?
                else {
                    anyhow::bail!("reservation")
                };
                if index == ordinal {
                    selected = Some(reservation);
                    break;
                }
                let PageOutcome::Deferred { retry_at_ms, .. } = database
                    .durable_fail(
                        &reservation,
                        FailureRecord {
                            class: FailureClass::Transient,
                            detail: "network interruption",
                            capture: None,
                            stopped_origin: None,
                            observed_at_ms: now,
                        },
                    )
                    .await?
                else {
                    anyhow::bail!("prior retry")
                };
                now = retry_at_ms;
            }
            let reservation = selected.context("final reservation")?;
            assert_eq!(reservation.ordinal, ordinal);
            let delay = Duration::from_millis(500);
            let fixture = WireFixture::start(FixtureKind::DelayedPage {
                status: 503,
                body: "temporary failure",
                delay,
            })
            .await?;
            let mut connection = transport(root.path(), &fixture, &database)?;
            let started = tokio::time::Instant::now();
            let fetched =
                page::fetch(&database, root.path(), &reservation, &mut connection, now).await?;
            let elapsed = i64::try_from(started.elapsed().as_millis())?;
            if ordinal == 3 {
                assert!(matches!(fetched, FetchDecision::Settled { outcome: PageOutcome::Exhausted { detail } } if detail.contains("acquisition attempt budget exhausted (3/3)")));
                assert!(matches!(page::reserve(&database, &request, now + scope.cooldown_ms * 10).await?, ReservationDecision::Settled { outcome: PageOutcome::Exhausted { .. } }));
                assert!(elapsed >= 500);
                connection.close().await?;
                assert_eq!(fixture.finish().await?.len(), 1);
                return Ok(());
            }
            let FetchDecision::Settled {
                outcome: PageOutcome::Deferred { retry_at_ms, cycle },
            } = fetched
            else {
                anyhow::bail!("deferred failure")
            };
            let wait = 1000;
            assert!(
                elapsed >= 500,
                "real transport must wait for delayed fixture"
            );
            assert!(
                retry_at_ms >= now + 500 + wait,
                "delay must begin after the delayed response, not at action entry"
            );
            assert!(
                retry_at_ms <= now + elapsed + wait,
                "completion time cannot exceed action return time"
            );
            assert_eq!(cycle, 1);
            assert!(matches!(
                page::reserve(&database, &request, retry_at_ms - 1).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Deferred { .. }
                }
            ));
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT attempts FROM scrape_pages WHERE id=$1")
                    .bind(request.binding.page_id)
                    .fetch_one(pool)
                    .await?,
                ordinal
            );
            let ReservationDecision::Reserved { reservation: next } =
                page::reserve(&database, &request, retry_at_ms).await?
            else {
                anyhow::bail!("due retry")
            };
            assert_eq!(next.total_attempts, ordinal + 1);
            assert_eq!(next.cycle, 1);
            assert_eq!(next.ordinal, 2);
            connection.close().await?;
            assert_eq!(fixture.finish().await?.len(), 1);
            Ok(())
        })
    })
    .await
}

#[test]
fn only_exact_uncaptured_invalid_interception_id_errors_are_transient_and_blocks_dominate() {
    let mut failure=crate::transport::Failure {
        blocked:false,retryable:false,capture:None,stopped_origin:None,
        detail:"browser interception failed: Error -32602: Invalid InterceptionId.: Error -32602: Invalid InterceptionId.".into(),
    };
    assert_eq!(page::classify(&failure), FailureClass::Transient);
    failure.blocked = true;
    assert_eq!(page::classify(&failure), FailureClass::Blocked);
    failure.blocked = false;
    failure.capture = Some(crate::model::Capture {
        url: target().url,
        status: 200,
        content_type: "text/html".into(),
        body: Vec::new(),
    });
    assert_eq!(page::classify(&failure), FailureClass::Rejected);
    failure.capture = None;
    failure.detail = "browser interception failed: Error -32602: malformed response".into();
    assert_eq!(page::classify(&failure), FailureClass::Rejected);
    failure.detail = "other operation failed: Error -32602: Invalid InterceptionId.".into();
    assert_eq!(page::classify(&failure), FailureClass::Rejected);
}

#[tokio::test]
async fn explicit_new_generation_recovers_exact_saved_cdp_failure_without_rewriting_rejected_history()
-> Result<()> {
    with_database(|pool| Box::pin(async move {
        let database=Database::durable_test_database(pool.clone());
        let mut scope=scope()?;
        let request=page::bind_target(&database,target(),&scope).await?;
        let detail="browser interception failed: Error -32602: Invalid InterceptionId.: Error -32602: Invalid InterceptionId.";
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error=$2 WHERE id=$1").bind(request.binding.page_id).bind(detail).execute(pool).await?;
        let now=chrono::Utc::now().timestamp_millis();
        let ReservationDecision::Reserved {reservation}=page::reserve(&database,&request,now).await? else {anyhow::bail!("legacy recovery")};
        assert_eq!(reservation.total_attempts,4);
        let old_outcome=database.durable_fail(&reservation,FailureRecord {class:FailureClass::Rejected,detail,capture:None,stopped_origin:None,observed_at_ms:now}).await?;
        assert!(matches!(old_outcome,PageOutcome::Rejected {..}));
        let old_record:serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1").bind(&request.binding.operation_id).fetch_one(pool).await?;
        assert!(matches!(page::reserve(&database,&request,now).await?,ReservationDecision::Settled {outcome:PageOutcome::Rejected {..}}));
        scope.run_generation="explicit-reviewed-cdp-recovery".into();
        let next=page::bind_target(&database,target(),&scope).await?;
        let ReservationDecision::Reserved {reservation:recovery}=page::reserve(&database,&next,now).await? else {anyhow::bail!("explicit new generation recovery")};
        assert_eq!(recovery.total_attempts,5);
        assert_eq!((recovery.cycle,recovery.ordinal),(1,1));
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1").bind(&request.binding.operation_id).fetch_one(pool).await?,old_record);
        assert_eq!(sqlx::query_scalar::<_,i32>("SELECT original_attempts FROM durable_page_operations WHERE operation_id=$1").bind(&next.binding.operation_id).fetch_one(pool).await?,4);
        assert_eq!(sqlx::query_scalar::<_,i32>("SELECT attempts FROM scrape_pages WHERE id=$1").bind(request.binding.page_id).fetch_one(pool).await?,5);
        Ok(())
    })).await
}
