use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};

use super::{super::evidence, journal, seed::Seeded, verify};
use crate::native_ops::recovery::{cli, io, runtime::Runtime};

pub(super) async fn run(
    runtime: &mut Runtime,
    seeded: &Seeded,
    fixture_endpoint: &str,
) -> Result<Value> {
    let registered = runtime
        .register_fixture(
            fixture_endpoint,
            "deadline-extension-fixture-registration.json",
        )
        .await?;
    let (status, route) = runtime
        .http
        .request(&runtime.admin, "/services/AthleticPage", None, None)
        .await?;
    ensure!(
        status == 200 && route.get("deployment_id") == registered.get("id"),
        "deadline extension fixture route mismatch"
    );
    let path = runtime
        .args
        .evidence
        .join("deadline-extension-request.json");
    io::save(&path, &serde_json::to_value(&seeded.request)?)?;
    let submission = submit(runtime, &path).await?;
    let root_id = io::invocation(&submission)?.to_owned();
    evidence::identifier(&root_id, "inv_")?;
    let (child_id, child_key) = child(runtime, &seeded.request.generation).await?;
    journal::wait_for_sleep(runtime, &child_id).await?;
    let restart = runtime.restart_active(&root_id).await?;
    let report = runtime
        .http
        .output(&runtime.ingress, &root_id, "progress")
        .await?;
    check_report(&report)?;
    check_duplicate(runtime, &path, &root_id, &report).await?;
    check_single_child(runtime, &child_key, &child_id).await?;
    let verified = verify::run(runtime, &root_id, &child_id, seeded).await?;
    Ok(
        json!({"root_invocation_id":root_id,"child_invocation_id":child_id,
        "same_root_after_restart":true,"one_child_after_duplicate_replay":true,
        "selected_page_request":verified.selected_request,"restart":restart,
        "operation_before":seeded.old_operation,"operation_after":verified.operation_after,
        "attempt_ids":verified.attempt_ids,"retry_due_after_old_deadline":true,
        "source_http_requests":verified.wire_requests,"report":report}),
    )
}

async fn submit(runtime: &Runtime, path: &std::path::Path) -> Result<Value> {
    cli::call(
        runtime,
        &[
            "submit",
            "--request",
            path.to_str()
                .context("deadline extension request path not UTF8")?,
        ],
    )
    .await
}

async fn child(runtime: &Runtime, generation: &str) -> Result<(String, String)> {
    let child_key = format!("{generation}-000000-page-000000");
    let rows = evidence::wait(
        runtime,
        &format!("SELECT id,status FROM sys_invocation WHERE target_service_name='AthleticPage' AND idempotency_key='{child_key}'"),
        |rows| rows.len() == 1,
    )
    .await?;
    let child_id = io::text(
        rows.first().context("deadline extension child absent")?,
        "/id",
    )?
    .to_owned();
    Ok((child_id, child_key))
}

fn check_report(report: &Value) -> Result<()> {
    ensure!(
        report.pointer("/progress/selected").and_then(Value::as_u64) == Some(1)
            && report.pointer("/progress/deferred").and_then(Value::as_u64) == Some(1)
            && report.get("continuation") == Some(&Value::Null),
        "deadline extension crawl did not finish one bounded deferred page"
    );
    Ok(())
}

async fn check_duplicate(
    runtime: &Runtime,
    path: &std::path::Path,
    root_id: &str,
    report: &Value,
) -> Result<()> {
    let duplicate = submit(runtime, path).await?;
    ensure!(
        io::invocation(&duplicate)? == root_id,
        "deadline extension replay changed root invocation identity"
    );
    ensure!(
        runtime
            .http
            .output(&runtime.ingress, root_id, "progress")
            .await?
            == *report,
        "deadline extension replay changed retained result"
    );
    Ok(())
}

async fn check_single_child(runtime: &Runtime, key: &str, child_id: &str) -> Result<()> {
    let children = evidence::query(
        runtime,
        &format!("SELECT id,status,idempotency_key FROM sys_invocation WHERE target_service_name='AthleticPage' AND idempotency_key='{key}'"),
    )
    .await?;
    ensure!(
        children.len() == 1
            && io::text(children.first().context("child absent")?, "/id")? == child_id,
        "deadline extension replay duplicated or replaced its child"
    );
    Ok(())
}
