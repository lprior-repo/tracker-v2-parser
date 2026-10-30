use super::reserve_configured;
use crate::{
    database::Database,
    durable::{
        pacing::test_support::with_database,
        page::{self, PageOutcome, ReservationDecision},
    },
    model::PageKind,
};
use anyhow::{Context, Result};
use serde_json::Value;
mod support;
use support::{config, legacy, request, snapshot};
const FAILURE: &str =
    "scanning source script boundaries: cannot safely classify computed script assignments";
const ADDRESS: &str = "https://schools.wiaawi.org/Directory/School/List";

#[tokio::test]
async fn reviewed_wiaa_repair_requires_new_bound_scope_preserves_settled_history_and_attempts()
-> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let now = chrono::Utc::now().timestamp_millis();
            let default = config(false)?;
            let original = request(
                &database,
                &default,
                ADDRESS,
                PageKind::Contacts,
                now,
                "original",
            )
            .await?;
            legacy(&database, &original, FAILURE, "failed").await?;
            let before = sqlx::query_scalar::<_, Value>("SELECT to_jsonb(p) FROM scrape_pages p")
                .fetch_one(pool)
                .await?;
            let rejected = reserve_configured(&database, &default, &original, now).await?;
            assert!(matches!(
                rejected,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Rejected { .. }
                }
            ));
            assert_eq!(
                sqlx::query_scalar::<_, Value>("SELECT to_jsonb(p) FROM scrape_pages p")
                    .fetch_one(pool)
                    .await?,
                before
            );
            let saved = sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
            )
            .bind(&original.binding.operation_id)
            .fetch_one(pool)
            .await?;
            let reviewed = config(true)?;
            let before_mismatch = snapshot(&database).await?;
            assert!(
                reserve_configured(&database, &reviewed, &original, now)
                    .await
                    .is_err()
            );
            assert_eq!(snapshot(&database).await?, before_mismatch);
            let next = request(
                &database,
                &reviewed,
                ADDRESS,
                PageKind::Contacts,
                now,
                "reviewed",
            )
            .await?;
            assert!(matches!(
                page::reserve(&database, &next, now).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Rejected { .. }
                }
            ));
            let after_strict = snapshot(&database).await?;
            assert!(matches!(
                reserve_configured(&database, &reviewed, &next, now).await?,
                ReservationDecision::Settled {
                    outcome: PageOutcome::Rejected { .. }
                }
            ));
            assert_eq!(snapshot(&database).await?, after_strict);
            let next = request(
                &database,
                &reviewed,
                ADDRESS,
                PageKind::Contacts,
                now,
                "reviewed-fresh",
            )
            .await?;
            let ReservationDecision::Reserved { reservation } =
                reserve_configured(&database, &reviewed, &next, now).await?
            else {
                anyhow::bail!("reviewed reservation missing")
            };
            assert_eq!(
                (
                    reservation.total_attempts,
                    reservation.cycle,
                    reservation.ordinal
                ),
                (4, 1, 1)
            );
            assert_eq!(
                sqlx::query_scalar::<_, i32>(
                    "SELECT original_attempts FROM durable_page_operations WHERE operation_id=$1"
                )
                .bind(&next.binding.operation_id)
                .fetch_one(pool)
                .await?,
                3
            );
            assert_eq!(
                sqlx::query_scalar::<_, Value>(
                    "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1"
                )
                .bind(&original.binding.operation_id)
                .fetch_one(pool)
                .await?,
                saved
            );
            assert_eq!(
                sqlx::query_scalar::<_, Vec<String>>("SELECT notes FROM scrape_pages")
                    .fetch_one(pool)
                    .await?,
                vec!["original evidence"]
            );
            let settled =
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(pool)
                    .await?;
            assert_eq!(settled, 1);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts")
                    .fetch_one(pool)
                    .await?,
                0
            );
            let before_replay = snapshot(&database).await?;
            let replay = reserve_configured(&database, &reviewed, &next, now).await?;
            let ReservationDecision::Reserved {
                reservation: replay,
            } = replay
            else {
                anyhow::bail!("replay reservation missing")
            };
            assert_eq!(replay.attempt_id, reservation.attempt_id);
            assert_eq!(snapshot(&database).await?, before_replay);
            let other = request(
                &database,
                &reviewed,
                ADDRESS,
                PageKind::Contacts,
                now,
                "another-owner",
            )
            .await?;
            let before_owner = snapshot(&database).await?;
            assert!(
                format!(
                    "{:#}",
                    reserve_configured(&database, &reviewed, &other, now)
                        .await
                        .err()
                        .context("ownership rejection")?
                )
                .contains("explicit handoff required")
            );
            assert_eq!(snapshot(&database).await?, before_owner);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn reviewed_wiaa_repair_never_bypasses_blocked_or_unrelated_legacy_failures() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let now = chrono::Utc::now().timestamp_millis();
            let reviewed = config(true)?;
            for (index, address, kind, detail, status) in [
                (
                    1,
                    ADDRESS,
                    PageKind::Contacts,
                    FAILURE.to_owned(),
                    "blocked",
                ),
                (
                    2,
                    "https://other.example/Directory/School/List",
                    PageKind::Contacts,
                    FAILURE.to_owned(),
                    "failed",
                ),
                (
                    3,
                    "https://schools.wiaawi.org/Directory/School/List?other=1",
                    PageKind::Contacts,
                    FAILURE.to_owned(),
                    "failed",
                ),
                (
                    4,
                    "https://wi.milesplit.com/teams/42-school",
                    PageKind::Team,
                    FAILURE.to_owned(),
                    "failed",
                ),
                (
                    5,
                    "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=B",
                    PageKind::Contacts,
                    "HTTP 403".to_owned(),
                    "failed",
                ),
                (
                    6,
                    "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=C",
                    PageKind::Contacts,
                    format!("{FAILURE} [challenge_marker=true]"),
                    "failed",
                ),
                (
                    7,
                    "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=D",
                    PageKind::Contacts,
                    "invalid required identity".to_owned(),
                    "failed",
                ),
            ] {
                let request = request(
                    &database,
                    &reviewed,
                    address,
                    kind,
                    now,
                    &format!("case-{index}"),
                )
                .await?;
                legacy(&database, &request, &detail, status).await?;
                let before = snapshot(&database).await?;
                let outcome = reserve_configured(&database, &reviewed, &request, now).await;
                if status == "blocked" {
                    assert!(
                        outcome
                            .err()
                            .context("blocked checkpoint")?
                            .to_string()
                            .contains("checkpoint is blocked")
                    );
                    assert_eq!(snapshot(&database).await?, before);
                } else {
                    assert!(matches!(
                        outcome?,
                        ReservationDecision::Settled {
                            outcome: PageOutcome::Rejected { .. }
                        }
                    ));
                    let page = sqlx::query_scalar::<_, Value>(
                        "SELECT to_jsonb(p) FROM scrape_pages p WHERE id=$1",
                    )
                    .bind(request.binding.page_id)
                    .fetch_one(pool)
                    .await?;
                    assert_eq!(page["attempts"], 3);
                    assert_eq!(page["last_error"], detail);
                }
            }
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_attempts")
                    .fetch_one(pool)
                    .await?,
                0
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_page_receipts")
                    .fetch_one(pool)
                    .await?,
                0
            );
            Ok(())
        })
    })
    .await
}
