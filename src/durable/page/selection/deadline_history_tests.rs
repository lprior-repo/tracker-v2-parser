use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    database::{Database, Job},
    durable::{
        model::DeadlineExtensionAuthorization,
        pacing::test_support::with_database,
        page::{self, PageRequest},
    },
};

use super::deadline_extension_tests::{config, scope, snapshot, target};

async fn owner(database: &Database, deadline: i64, suffix: u32) -> Result<PageRequest> {
    let mut target = target();
    target.url = format!("https://wi.milesplit.com/teams/{suffix}-history-fixture");
    database
        .enqueue(std::slice::from_ref(&target), false)
        .await?;
    let id = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE cursor=$1")
        .bind(serde_json::to_value(&target)?)
        .fetch_one(database.pool())
        .await?;
    let request = page::request(Job { id, target }, &scope(&config()?, deadline)?)?;
    let reserved = page::reserve(database, &request, deadline - 500).await?;
    assert!(matches!(
        reserved,
        page::ReservationDecision::Reserved { .. }
    ));
    Ok(request)
}

#[tokio::test]
async fn explicit_history_reuses_all_approved_owner_epochs_without_changing_ledger() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let now = chrono::Utc::now().timestamp_millis();
            let first = owner(&database, now + 1_000, 731).await?;
            let second = owner(&database, now + 2_000, 732).await?;
            let third = owner(&database, now + 3_000, 733).await?;
            let before = snapshot(&database).await?;
            let selected_scope = scope(&config()?, now + 60_000)?;
            let authority = DeadlineExtensionAuthorization {
                previous_deadline_ms: now + 3_000,
                effective_deadline_ms: selected_scope.deadline_ms,
                earlier_deadlines_ms: vec![now + 1_000, now + 2_000],
            };
            for original in [first, second, third] {
                let replay = database
                    .durable_request_with_deadline_extension(
                        Job {
                            id: original.binding.page_id(),
                            target: original.target.clone(),
                        },
                        &selected_scope,
                        &authority,
                    )
                    .await?;
                assert_eq!(replay.binding, original.binding);
                let extension = replay
                    .deadline_extension
                    .context("history extension absent")?;
                assert_eq!(
                    extension.previous_deadline_ms,
                    original.binding.scope().deadline_ms
                );
                assert_eq!(
                    extension.owner_operation_id,
                    original.binding.operation_id()
                );
            }
            assert_eq!(snapshot(&database).await?, before);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn absent_or_unordered_history_cannot_authorize_an_older_owner() -> Result<()> {
    with_database(|pool| {
        Box::pin(async move {
            let database = Database::durable_test_database(pool.clone());
            let now = chrono::Utc::now().timestamp_millis();
            let original = owner(&database, now + 1_000, 731).await?;
            let before = snapshot(&database).await?;
            let selected_scope = scope(&config()?, now + 60_000)?;
            for earlier in [vec![], vec![now + 1_001], vec![now + 2_000, now + 1_000]] {
                let error = database
                    .durable_request_with_deadline_extension(
                        Job {
                            id: original.binding.page_id(),
                            target: original.target.clone(),
                        },
                        &selected_scope,
                        &DeadlineExtensionAuthorization {
                            previous_deadline_ms: now + 3_000,
                            effective_deadline_ms: selected_scope.deadline_ms,
                            earlier_deadlines_ms: earlier,
                        },
                    )
                    .await;
                assert!(error.is_err());
                assert_eq!(snapshot(&database).await?, before);
            }
            Ok(())
        })
    })
    .await
}

#[test]
fn legacy_deadline_authority_json_remains_byte_compatible() -> Result<()> {
    let old = json!({"previous_deadline_ms":100,"effective_deadline_ms":200});
    let authority: DeadlineExtensionAuthorization = serde_json::from_value(old.clone())?;
    assert!(authority.earlier_deadlines_ms.is_empty());
    assert_eq!(serde_json::to_value(authority)?, old);
    Ok(())
}
