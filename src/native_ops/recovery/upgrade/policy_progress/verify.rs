use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{
    super::{evidence, handoff},
    seed::Seeded,
};
use crate::native_ops::recovery::{io, runtime::Runtime};

pub(super) async fn run(
    runtime: &Runtime,
    seeded: &Seeded,
    root: &str,
    first: &Value,
    second: &Value,
) -> Result<Value> {
    let first_selected = first
        .pointer("/progress/selected")
        .and_then(Value::as_u64)
        .context("first policy progress absent")?;
    let second_selected = second
        .pointer("/progress/selected")
        .and_then(Value::as_u64)
        .context("second policy progress absent")?;
    ensure!(
        first_selected == 2 && second_selected == 0,
        "policy work was counted as a page acquisition: first={first_selected}, second={second_selected}"
    );
    ensure!(
        first.pointer("/progress/queue").and_then(Value::as_str) == Some("processing")
            && first.get("stopped_reason") == Some(&Value::Null),
        "policy progress at the turn cap falsely completed the queue"
    );
    ensure!(
        second.get("continuation") == Some(&Value::Null),
        "policy proof exceeded its unchanged batch budget"
    );
    ensure!(
        handoff::checkpoints(runtime, &seeded.pages).await? == seeded.checkpoints,
        "policy proof rewrote original checkpoints or attempt counters"
    );
    let generation = seeded.request.generation();
    ensure!(
        generation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid owned policy generation"
    );
    let scope = seeded.request.page_scope()?;
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM durable_page_operations WHERE run_generation=$1 AND outcome->>'detail'='Public-results scope excludes a published robots restriction or ambiguous route'),(SELECT count(*) FROM durable_page_attempts a JOIN durable_page_operations d USING(operation_id) WHERE d.run_generation=$1),(SELECT count(*) FROM durable_wire_requests w JOIN durable_page_operations d USING(operation_id) WHERE d.run_generation=$1)")
        .bind(&scope.run_generation).fetch_one(&runtime.pool).await?;
    ensure!(
        counts == (i64::from(seeded.exclusions), 0, 0),
        "policy proof lost exclusions or admitted source attempts"
    );
    let children=evidence::query(runtime,&format!("SELECT id,idempotency_key,status FROM sys_invocation WHERE target_service_name='AthleticPage' AND idempotency_key LIKE '{generation}-%' ORDER BY idempotency_key")).await?;
    ensure!(
        children.len() == 2
            && children
                .iter()
                .all(|c| c.get("status").and_then(Value::as_str) == Some("completed")),
        "policy turns duplicated or failed to drain page children"
    );
    let journal = evidence::journal(runtime, root).await?;
    let entries = journal
        .iter()
        .map(evidence::decode)
        .collect::<Result<Vec<_>>>()?;
    let progress = entries
        .iter()
        .filter_map(|e| e.pointer("/Notification/Completion/Run"))
        .filter(|v| evidence::contains_json_value(v, &json!({"selection":"policy_advanced"})))
        .count();
    ensure!(progress > 0, "policy progress was not durably checkpointed");
    let expected = [
        format!("{generation}-000000-page-000000"),
        format!("{generation}-000000-page-000002"),
    ];
    ensure!(
        children
            .iter()
            .zip(expected.iter())
            .all(
                |(child, key)| child.get("idempotency_key").and_then(Value::as_str)
                    == Some(key.as_str())
            ),
        "policy progress reused a page idempotency index"
    );
    io::save(
        &runtime
            .args
            .evidence
            .join(format!("{generation}-journal.json")),
        &json!(journal),
    )?;
    Ok(
        json!({"root":root,"first_report":first,"continuation_report":second,"exclusions":counts.0,"acquisition_attempts":counts.1,"source_http_requests":counts.2,"children":children,"checkpoints_unchanged":true,"policy_progress_checkpointed":true,"input":seeded.request}),
    )
}
