use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::{fixtures, io, runtime::Runtime};

pub(super) async fn prove(runtime: &Runtime, identity: &str) -> Result<Value> {
    let before = fixtures::domain(runtime).await?;
    ensure!(
        before.get("source_wire_requests").and_then(Value::as_u64) == Some(0),
        "capacity proof must start without source requests"
    );
    let root = runtime.args.evidence.join("capacity-artifacts");
    let delivered = runtime.args.evidence.join("capacity-delivered.xlsx");
    let mut worker =
        crate::durable::capacity_proof::Worker::start(&runtime.database, &root, &delivered).await?;
    let proof = capacity_invocation(runtime, &mut worker, identity, &root, &delivered).await;
    let cleanup = worker.stop().await;
    let restored = runtime
        .http
        .request(
            &runtime.admin,
            "/deployments",
            Some(&json!({"uri":runtime.endpoint,"force":true})),
            None,
        )
        .await;
    cleanup?;
    let (status, registration) = restored?;
    ensure!(
        matches!(status, 200 | 201),
        "original worker restoration failed HTTP {status}"
    );
    io::save(
        &runtime
            .args
            .evidence
            .join("capacity-original-worker-restored.json"),
        &registration,
    )?;
    let (status, service) = runtime
        .http
        .request(&runtime.admin, "/services/AthleticOperations", None, None)
        .await?;
    ensure!(
        status == 200 && io::text(&service, "/deployment_id")? == io::text(&registration, "/id")?,
        "original worker routing was not restored"
    );
    let mut proof = proof?;
    ensure!(
        fixtures::domain(runtime).await? == before,
        "capacity export changed database facts"
    );
    proof
        .as_object_mut()
        .context("capacity proof must be object")?
        .insert("database_facts_unchanged".into(), json!(true));
    io::save(
        &runtime.args.evidence.join("capacity-retry-proof.json"),
        &proof,
    )?;
    Ok(proof)
}

async fn capacity_invocation(
    runtime: &Runtime,
    worker: &mut crate::durable::capacity_proof::Worker,
    identity: &str,
    root: &std::path::Path,
    delivered: &std::path::Path,
) -> Result<Value> {
    let (status, registration) = runtime
        .http
        .request(
            &runtime.admin,
            "/deployments",
            Some(&json!({"uri":worker.endpoint()})),
            None,
        )
        .await?;
    ensure!(
        matches!(status, 200 | 201),
        "capacity worker registration failed HTTP {status}"
    );
    io::save(
        &runtime.args.evidence.join("capacity-registration.json"),
        &registration,
    )?;
    let mut config = fixtures::configuration();
    *config
        .get_mut("states")
        .context("capacity config states absent")? = json!(["MI"]);
    let request = json!({"operation_id":format!("capacity-{identity}"),"spec":{"kind":"export","config":config}});
    let (id, _) = runtime.http.submit(&runtime.ingress, &request).await?;
    let retry = capacity_retry(runtime, &id).await?;
    ensure!(
        !delivered.try_exists()? && !root.join("current-export.json").try_exists()?,
        "capacity rejection unexpectedly published a workbook"
    );
    let before = capacity_journal(runtime, &id, "capacity-journal-before").await?;
    worker.release()?;
    let result = runtime
        .http
        .output(&runtime.ingress, &id, "receipt")
        .await?;
    let (duplicate, _) = runtime.http.submit(&runtime.ingress, &request).await?;
    ensure!(
        duplicate == id
            && runtime
                .http
                .output(&runtime.ingress, &duplicate, "receipt")
                .await?
                == result,
        "capacity retry changed invocation or duplicate output"
    );
    let after = capacity_journal(runtime, &id, "capacity-journal-after").await?;
    ensure!(
        before == after,
        "completed capacity export journal steps changed during retry"
    );
    let artifact = verify_delivery(&result, root, delivered)?;
    Ok(
        json!({"invocation_id":id,"observed_retry":retry,"same_invocation_completed":true,"duplicate_idempotent":true,"completed_journal_steps_unchanged":true,"held_blocking_stages":8,"source_requests":0,"delivery_verified":true,"immutable_workbook":artifact,"original_worker_restored":true,"owned_tasks_drained":true}),
    )
}

fn verify_delivery(
    result: &Value,
    root: &std::path::Path,
    delivered: &std::path::Path,
) -> Result<Value> {
    use calamine::Reader;

    let receipt = io::artifact(
        result.get("receipt").context("capacity receipt absent")?,
        root,
    )?;
    let completed: Value = serde_json::from_slice(&io::read(&receipt, io::JSON_LIMIT)?)?;
    let artifact = completed
        .pointer("/publication/workbook")
        .context("capacity workbook absent")?;
    let workbook = io::artifact(artifact, root)?;
    ensure!(
        io::file_digest(&workbook, crate::export::MAX_EXPORT_BYTES)?
            == io::file_digest(delivered, crate::export::MAX_EXPORT_BYTES)?,
        "capacity delivery differs from immutable workbook"
    );
    let book: calamine::Xlsx<_> = calamine::open_workbook(delivered)?;
    ensure!(
        book.sheet_names() == ["Athletes"],
        "capacity workbook sheet layout invalid"
    );
    Ok(artifact.clone())
}

async fn capacity_retry(runtime: &Runtime, id: &str) -> Result<Value> {
    let until = io::deadline(std::time::Duration::from_mins(1))?;
    stream::iter(0..240)
        .map(Ok::<_, anyhow::Error>)
        .try_fold(None, |found, _| async move {
            if found.is_some() {
                return Ok(found);
            }
            ensure!(
                tokio::time::Instant::now() < until,
                "capacity retry was not observed within60seconds"
            );
            let (status, value) = runtime
                .http
                .request(
                    &runtime.admin,
                    "/query",
                    Some(&json!({"query":format!("SELECT * FROM sys_invocation WHERE id='{id}'")})),
                    None,
                )
                .await?;
            ensure!(status == 200, "capacity introspection failed HTTP {status}");
            let observed = io::array(&value, "/rows")?
                .iter()
                .find(|row| {
                    serde_json::to_string(row).is_ok_and(|text| {
                        text.contains("owned blocking stage capacity of 8 is exhausted")
                    })
                })
                .cloned();
            if observed.is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Ok(observed)
        })
        .await?
        .context("Restate did not expose the expected retryable capacity failure")
}

async fn capacity_journal(runtime: &Runtime, id: &str, label: &str) -> Result<Vec<Value>> {
    let rows = runtime
        .query(
            &format!("SELECT * FROM sys_journal WHERE id='{id}' ORDER BY index"),
            label,
        )
        .await?;
    let names = ["freeze-export-input", "read-frozen-chunk-plan"];
    let completed = rows
        .into_iter()
        .filter(|row| {
            serde_json::to_string(row)
                .is_ok_and(|body| names.iter().any(|name| body.contains(name)))
        })
        .collect::<Vec<_>>();
    ensure!(
        completed.len() == 2
            && names.iter().all(|name| completed
                .iter()
                .filter(|row| serde_json::to_string(row).is_ok_and(|body| body.contains(name)))
                .count()
                == 1),
        "capacity export did not retain exactly one completed journal entry per prior stage"
    );
    Ok(completed)
}
