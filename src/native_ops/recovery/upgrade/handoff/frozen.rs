use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::Value;

use super::{
    super::{
        super::{cli, io, process, runtime::Runtime},
        evidence,
    },
    checkpoints,
};
use crate::durable::model::{CrawlPhase, CrawlRequest};

pub(super) struct FrozenRoot {
    pub id: String,
    pub prefix: Vec<Value>,
    pub request: CrawlRequest,
    pub input_bytes: Vec<u8>,
    pub pages: Vec<i64>,
    pub checkpoints: Value,
}

pub(super) async fn pause(
    runtime: &mut Runtime,
    identity: &str,
    previous: &Value,
) -> Result<FrozenRoot> {
    let (pages, request) = prepare(runtime, identity).await?;
    let checkpoints = checkpoints(runtime, &pages).await?;
    let frozen = serde_json::to_value(&request)?;
    ensure!(
        frozen.get("scheduler_version").is_none(),
        "historical request must omit scheduler version"
    );
    let path = runtime.args.evidence.join("historical-request.json");
    io::save(&path, &frozen)?;
    let mut lock = runtime.pool.begin().await?;
    sqlx::query("LOCK TABLE athletes IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await?;
    let arguments = process::args(&[
        "durable",
        "submit",
        "--request",
        path.to_str().context("request path not UTF8")?,
        "--ingress",
        &runtime.ingress,
    ]);
    let submission = process::Owned::capture(
        runtime
            .args
            .previous_worker
            .as_ref()
            .context("previous worker absent")?,
        &arguments,
        &runtime.args.evidence,
        &runtime.database,
    )
    .await?;
    let id = io::invocation(&submission)?.to_owned();
    evidence::identifier(&id, "inv_")?;
    observe_partial(runtime, &id, previous).await?;
    lock.rollback().await?;
    runtime.stop_worker().await?;
    let prefix = evidence::journal(runtime, &id).await?;
    ensure!(
        !serde_json::to_string(&prefix)?.contains("OneWayCall"),
        "historical root sent work before its same-invocation handoff"
    );
    let children = evidence::query(runtime, "SELECT id FROM sys_invocation WHERE target_service_name='AthleticPage' AND status<>'completed'").await?;
    ensure!(
        children.is_empty(),
        "historical predecessor has unfinished owned children"
    );
    let (input_bytes, input) = evidence::input(&prefix)?;
    ensure!(input == frozen, "historical input changed before migration");
    evidence::no_attempts(runtime).await?;
    Ok(FrozenRoot {
        id,
        prefix,
        request,
        input_bytes,
        pages,
        checkpoints,
    })
}

async fn prepare(runtime: &Runtime, identity: &str) -> Result<(Vec<i64>, CrawlRequest)> {
    let pages = stream::iter((942_800..942_807).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut pages, school| async move {
            pages.push(cli::seed(runtime, "IA", school).await?);
            Ok(pages)
        })
        .await?;
    let path = cli::config(runtime, "IA", 3, 3, "historical-config.toml")?;
    Ok((
        pages,
        CrawlRequest {
            generation: format!("upgrade-{identity}"),
            config: crate::config::Config::load(&path)?,
            deadline_ms: chrono::Utc::now()
                .timestamp_millis()
                .checked_add(3_600_000)
                .context("upgrade deadline overflow")?,
            cooldown_ms: 15_000,
            batch: 0,
            max_batches: 2,
            phase: CrawlPhase::SchoolChains,
            inherited_retry_campaigns: vec![format!("ancestor-{identity}")],
            scheduler_version: None,
        },
    ))
}

async fn observe_partial(runtime: &Runtime, id: &str, previous: &Value) -> Result<()> {
    let partial = evidence::wait(
        runtime,
        &format!("SELECT index,entry_json FROM sys_journal WHERE id='{id}' ORDER BY index"),
        |rows| {
            rows.iter().any(|row| {
                evidence::decode(row).is_ok_and(|entry| {
                    entry.pointer("/Command/Run/name").and_then(Value::as_str)
                        == Some("select-page-000000")
                })
            })
        },
    )
    .await?;
    ensure!(
        !serde_json::to_string(&partial)?.contains("OneWayCall"),
        "historical root sent work before pause"
    );
    let status = runtime
        .patch_owned(&format!("/invocations/{id}/pause"))
        .await?;
    ensure!(
        matches!(status, 200 | 202),
        "owned root pause failed HTTP {status}"
    );
    let paused = evidence::wait(
        runtime,
        &format!("SELECT id,status,pinned_deployment_id FROM sys_invocation WHERE id='{id}'"),
        |rows| {
            rows.first()
                .is_some_and(|row| row.get("status").and_then(Value::as_str) == Some("paused"))
        },
    )
    .await?;
    ensure!(
        paused
            .first()
            .and_then(|row| row.get("pinned_deployment_id"))
            == previous.get("id"),
        "historical root was not pinned to the verified old deployment"
    );
    Ok(())
}
