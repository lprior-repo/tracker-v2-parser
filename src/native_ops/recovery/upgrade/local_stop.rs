mod seed;
mod verify;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::evidence;
use crate::native_ops::recovery::{io, runtime::Runtime};

pub(super) async fn prove(runtime: &mut Runtime, identity: &str) -> Result<Value> {
    runtime.stop_worker().await?;
    runtime.rotate_worker_port()?;
    runtime.start_previous_worker().await?;
    let previous = runtime
        .register_named("local-stop-previous-registration.json")
        .await?;
    let seeded = seed::prepare(runtime, identity).await?;
    let before = verify::reserved(runtime, &seeded.input).await?;
    let storage = seed::retain(runtime, &seeded.input, &before).await?;
    let old_key = format!("local-stop-original-{identity}");
    let original = submit(runtime, &seeded.input, &old_key).await?;
    evidence::wait(
        runtime,
        &format!("SELECT status FROM sys_invocation WHERE id='{original}'"),
        |rows| {
            rows.first()
                .and_then(|row| row.get("status"))
                .and_then(Value::as_str)
                == Some("completed")
        },
    )
    .await?;
    let (status, previous_result) = runtime
        .http
        .request(
            &runtime.ingress,
            &format!("/restate/output/{original}"),
            None,
            None,
        )
        .await?;
    ensure!(
        (status == 500
            && previous_result
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("blocked page requiring human review")))
            || (status == 200
                && previous_result.get("state").and_then(Value::as_str) == Some("completed")),
        "previous worker did not preserve an explicitly recognized local-stop outcome"
    );
    let prefix = evidence::journal(runtime, &original).await?;
    let checkpoints = verify::checkpoints(runtime, &seeded.stops).await?;
    runtime.stop_worker().await?;
    runtime.rotate_worker_port()?;
    runtime.start_worker().await?;
    let registered = runtime
        .register_named("local-stop-upgraded-registration.json")
        .await?;
    let recovery_key = format!("local-stop-recover-{identity}");
    let recovered = submit(runtime, &seeded.input, &recovery_key).await?;
    let outcome = runtime
        .http
        .output(&runtime.ingress, &recovered, "state")
        .await?;
    ensure!(
        outcome.get("state").and_then(Value::as_str) == Some("completed"),
        "same reserved operation did not complete"
    );
    ensure!(
        submit(runtime, &seeded.input, &recovery_key).await? == recovered,
        "unknown recovery reconciliation duplicated invocation"
    );
    ensure!(
        runtime
            .http
            .output(&runtime.ingress, &recovered, "state")
            .await?
            == outcome,
        "recovery replay changed retained outcome"
    );
    let after = verify::completed(runtime, &seeded.input, &before).await?;
    let export = verify::export(runtime, &seeded.input, identity).await?;
    ensure!(
        verify::checkpoints(runtime, &seeded.stops).await? == checkpoints,
        "recovery changed blocked checkpoints"
    );
    ensure!(
        evidence::journal(runtime, &original).await? == prefix,
        "original invocation journal changed"
    );
    let proof = json!({"status":"passed","original_invocation":original,"recovery_invocation":recovered,
        "previous_deployment":previous.get("id"),"new_deployment":registered.get("id"),
        "previous_http_status":status,"previous_result":previous_result,"capture_storage":storage,"exact_page_execution":seeded.input,"reservation_before":before,
        "reservation_after":after,"same_operation_and_attempt":true,"attempts_not_increased":true,
        "original_journal_unchanged":true,"original_failed_journal_unchanged":status==500,"original_prefix_entries":prefix.len(),
        "original_prefix_sha256":io::digest(&serde_json::to_vec(&prefix)?),"blocked_checkpoints_unchanged":true,
        "recovery_reconciliation_same_invocation":true,"recovery_source_http_requests":0,"outcome":outcome,"recovered_operation_export":export});
    io::save(
        &runtime
            .args
            .evidence
            .join("local-stop-reserved-recovery.json"),
        &proof,
    )?;
    Ok(proof)
}

async fn submit(runtime: &Runtime, input: &Value, key: &str) -> Result<String> {
    let (status, response) = runtime
        .http
        .request(
            &runtime.ingress,
            "/restate/send/AthleticPage/handle",
            Some(input),
            Some(key),
        )
        .await?;
    ensure!(
        matches!(status, 200 | 202),
        "owned reserved recovery submit HTTP{status}"
    );
    let id = io::invocation(&response)?.to_owned();
    evidence::identifier(&id, "inv_").context("invalid reserved recovery invocation")?;
    Ok(id)
}
