use anyhow::Result;

use super::{page, scope, target};
use crate::{
    database::Database,
    durable::{
        pacing::test_support::with_database,
        page::{FailureClass, FailureRecord, PageOutcome, ReservationDecision},
    },
    transport::Failure,
};

const DIRECT: &str =
    "browser interception failed: browser request admission budget exceeded: no available capacity";
const CLEANUP: &str = "page cleanup also failed: browser acquisition incomplete: admission ended before quiescence: channel closed: browser interception failed: browser request admission budget exceeded: no available capacity";

#[test]
fn given_exact_uncaptured_queue_saturation_when_classified_then_only_capacity_is_transient() {
    for detail in [DIRECT, CLEANUP] {
        let mut failure = Failure {
            blocked: false,
            retryable: false,
            capture: None,
            stopped_origin: None,
            detail: detail.into(),
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
        for changed in [
            format!("HTTP 403: {detail}"),
            format!(
                "browser acquisition incomplete: a requested destination was blocked: {detail}"
            ),
            format!("{detail}; CAPTCHA"),
            detail.replace("no available capacity", "channel closed"),
            detail.replace("browser request admission", "unrecognized admission"),
        ] {
            failure.detail = changed.into();
            assert_eq!(page::classify(&failure), FailureClass::Rejected);
        }
    }
}

#[tokio::test]
async fn given_saved_capacity_rejection_when_new_scope_runs_then_history_is_preserved() -> Result<()>
{
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let mut scope = scope()?;
            let request = page::bind_target(&database, target(), &scope).await?;
            let now = chrono::Utc::now().timestamp_millis();
            let ReservationDecision::Reserved { reservation } =
                page::reserve(&database, &request, now).await?
            else {
                anyhow::bail!("initial reservation")
            };
            let original = database
                .durable_fail(
                    &reservation,
                    FailureRecord {
                        class: FailureClass::Rejected,
                        detail: CLEANUP,
                        capture: None,
                        stopped_origin: None,
                        observed_at_ms: now,
                    },
                )
                .await?;
            assert!(matches!(original, PageOutcome::Rejected { .. }));
            let before: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&request.binding.operation_id)
            .fetch_one(pool)
            .await?;
            assert!(matches!(
                page::reserve(&database, &request, now).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Rejected { .. }
                }
            ));
            scope.run_generation = "reviewed-contact-capacity-recovery".into();
            let next = page::bind_target(&database, target(), &scope).await?;
            let ReservationDecision::Reserved { reservation: retry } =
                page::reserve(&database, &next, now).await?
            else {
                anyhow::bail!("fresh capacity recovery reservation")
            };
            assert_eq!(
                (retry.total_attempts, retry.cycle, retry.ordinal),
                (2, 1, 1)
            );
            let after: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&request.binding.operation_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(before, after);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(pool)
                    .await?,
                2
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(pool)
                    .await?,
                0
            );
            Ok(())
        })
    })
    .await
}
