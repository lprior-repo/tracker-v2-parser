use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

use super::{super::evidence, journal, seed::Seeded};
use crate::native_ops::recovery::runtime::Runtime;

pub(super) struct Verified {
    pub selected_request: Value,
    pub operation_after: Value,
    pub attempt_ids: Vec<String>,
    pub wire_requests: i64,
}

pub(super) async fn run(
    runtime: &Runtime,
    root_id: &str,
    child_id: &str,
    seeded: &Seeded,
) -> Result<Verified> {
    let selected_request = retained_request(runtime, root_id, child_id, seeded).await?;
    let operation_id = seeded
        .old_operation
        .get("operation_id")
        .and_then(Value::as_str)
        .context("old operation ID absent")?;
    let (operation_after, attempt_ids, wire_requests) =
        database_evidence(runtime, operation_id, seeded).await?;
    Ok(Verified {
        selected_request,
        operation_after,
        attempt_ids,
        wire_requests,
    })
}

async fn retained_request(
    runtime: &Runtime,
    root_id: &str,
    child_id: &str,
    seeded: &Seeded,
) -> Result<Value> {
    let child_rows = evidence::journal(runtime, child_id).await?;
    let (_, child_input) = evidence::input(&child_rows)?;
    let selected_request = child_input
        .get("request")
        .context("page child request absent")?;
    ensure!(
        selected_request.pointer("/binding/operation_id")
            == seeded.old_operation.get("operation_id")
            && selected_request
                .pointer("/binding/scope/deadline_ms")
                .and_then(Value::as_i64)
                == Some(seeded.previous_deadline_ms)
            && selected_request.pointer("/deadline_extension/owner_operation_id")
                == seeded.old_operation.get("operation_id")
            && selected_request
                .pointer("/deadline_extension/previous_deadline_ms")
                .and_then(Value::as_i64)
                == Some(seeded.previous_deadline_ms)
            && selected_request
                .pointer("/deadline_extension/effective_deadline_ms")
                .and_then(Value::as_i64)
                == Some(seeded.request.deadline_ms),
        "replayed child input did not retain the exact authorized immutable binding"
    );
    let root_rows = evidence::journal(runtime, root_id).await?;
    ensure!(
        journal::selected_run_output_contains(&root_rows, selected_request)?,
        "recorded select-page Run output lacks the canonical extended request"
    );
    Ok(selected_request.clone())
}

async fn database_evidence(
    runtime: &Runtime,
    operation_id: &str,
    seeded: &Seeded,
) -> Result<(Value, Vec<String>, i64)> {
    let operation_after: Value = sqlx::query_scalar(
        "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
    )
    .bind(operation_id)
    .fetch_one(&runtime.pool)
    .await?;
    ensure!(
        operation_after.get("binding") == seeded.old_operation.get("binding"),
        "deadline extension mutated the retained operation binding"
    );
    ensure!(
        operation_after.get("original_attempts") == seeded.old_operation.get("original_attempts"),
        "deadline extension reset original attempt accounting"
    );
    ensure!(
        operation_after
            .get("retry_at_ms")
            .and_then(Value::as_i64)
            .is_some_and(
                |due| due > seeded.previous_deadline_ms && due < seeded.request.deadline_ms
            ),
        "extended retry deadline did not govern the retained operation"
    );
    let attempt_ids = sqlx::query_scalar(
        "SELECT attempt_id FROM durable_page_attempts WHERE operation_id=$1 ORDER BY cycle,ordinal",
    )
    .bind(operation_id)
    .fetch_all(&runtime.pool)
    .await?;
    ensure!(
        attempt_ids.len() == 2
            && attempt_ids
                .first()
                .is_some_and(|id: &String| id == &seeded.old_attempt_id),
        "deadline extension did not preserve and increment cumulative attempts exactly once"
    );
    let wire_requests: i64 =
        sqlx::query_scalar("SELECT count(*) FROM durable_wire_requests WHERE operation_id=$1")
            .bind(operation_id)
            .fetch_one(&runtime.pool)
            .await?;
    ensure!(
        wire_requests == 0,
        "deadline extension proof emitted provider HTTP"
    );
    Ok((operation_after, attempt_ids, wire_requests))
}
