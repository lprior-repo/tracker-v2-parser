mod frozen;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{
    super::{cli, io, runtime::Runtime},
    evidence,
};

pub(super) async fn prove(
    runtime: &mut Runtime,
    identity: &str,
    previous: &Value,
) -> Result<Value> {
    let root = frozen::pause(runtime, identity, previous).await?;
    runtime.rotate_worker_port()?;
    runtime.start_worker().await?;
    let registered = runtime.register_named("upgraded-registration.json").await?;
    let deployment = io::text(&registered, "/id")?;
    evidence::identifier(deployment, "dp_")?;
    ensure!(
        Some(deployment) != previous.get("id").and_then(Value::as_str),
        "old and new workers must have different deployments"
    );
    ensure!(
        evidence::journal(runtime, &root.id).await? == root.prefix,
        "historical journal changed while drained"
    );
    let status = runtime
        .patch_owned(&format!(
            "/invocations/{}/resume?deployment={deployment}",
            root.id
        ))
        .await?;
    ensure!(status == 200, "owned root migration failed HTTP {status}");
    let report = runtime
        .http
        .output(&runtime.ingress, &root.id, "progress")
        .await?;
    let continuation = io::text(&report, "/continuation")?;
    let next = cli::continuation(runtime, continuation).await?;
    let retained = evidence::journal(runtime, &root.id).await?;
    ensure!(
        retained.get(..root.prefix.len()) == Some(root.prefix.as_slice()),
        "old completed journal prefix changed after migration"
    );
    ensure!(
        evidence::input(&retained)?.0 == root.input_bytes,
        "historical exact input bytes changed after migration"
    );
    let child_input = continuation_input(runtime, continuation).await?;
    let expected = root
        .request
        .next(crate::durable::model::CrawlPhase::SchoolChains)?
        .upgraded_scheduler();
    ensure!(
        child_input == serde_json::to_value(expected)?,
        "future continuation changed lineage, configuration, deadline or scheduler version"
    );
    ensure!(
        checkpoints(runtime, &root.pages).await? == root.checkpoints,
        "upgrade changed original failed checkpoints"
    );
    let attempts = evidence::no_attempts(runtime).await?;
    let proof = json!({"same_invocation_id":root.id,"historical_input_sha256":io::digest(&root.input_bytes),
        "historical_input":root.request,"previous_deployment":previous.get("id"),"new_deployment":deployment,
        "prefix_rows":root.prefix.len(),"prefix_sha256":io::digest(&serde_json::to_vec(&root.prefix)?),
        "prefix_unchanged":true,"input_bytes_unchanged":true,"future_continuation_input":child_input,
        "report":report,"continuation_report":next,"checkpoint_records_unchanged":true,"attempts":attempts});
    io::save(
        &runtime.args.evidence.join("historical-prefix.json"),
        &json!(root.prefix),
    )?;
    io::save(&runtime.args.evidence.join("handoff-proof.json"), &proof)?;
    Ok(proof)
}

async fn continuation_input(runtime: &Runtime, continuation: &str) -> Result<Value> {
    ensure!(
        continuation.len() < 128
            && continuation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "owned continuation key invalid"
    );
    let rows = evidence::query(
        runtime,
        &format!("SELECT id FROM sys_invocation WHERE target='AthleticCrawl/{continuation}/run'"),
    )
    .await?;
    ensure!(rows.len() == 1, "continuation invocation must be unique");
    let child = io::text(
        rows.first().context("continuation invocation absent")?,
        "/id",
    )?;
    let rows = evidence::journal(runtime, child).await?;
    Ok(evidence::input(&rows)?.1)
}

pub(super) async fn checkpoints(runtime: &Runtime, ids: &[i64]) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p WHERE id=ANY($1)",
    )
    .bind(ids)
    .fetch_one(&runtime.pool)
    .await?)
}
