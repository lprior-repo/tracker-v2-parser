use std::{ffi::OsString, path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::{io, process, runtime::Runtime};

pub(super) const BOUNDED: &str = "bounded batch count reached; retained checkpoints are resumable";

pub(super) async fn call(runtime: &Runtime, arguments: &[&str]) -> Result<Value> {
    let mut args = process::args(&["durable"]);
    args.extend(process::args(arguments));
    args.extend([
        OsString::from("--ingress"),
        OsString::from(&runtime.ingress),
    ]);
    process::Owned::capture(
        &runtime.args.worker,
        &args,
        &runtime.args.evidence,
        &runtime.database,
    )
    .await
}

pub(super) fn config(
    runtime: &Runtime,
    state: &str,
    pages: usize,
    concurrency: usize,
    name: &str,
) -> Result<PathBuf> {
    let config = json!({"states":[state],"graduation_years":[2027],"season":2026,"limits":{"requests_per_second":2.0,"max_pages":pages,"max_attempts":3,"concurrency":concurrency},"sources":{"athleticnet":{"enabled":false,"transport":"http"},"milesplit":{"enabled":true,"transport":"http"}},"export":{"layout":"single_sheet","rows_per_file":2000,"per_athlete_sheets":false},"acquisition":{"mode":"public_results","profile_coverage_only":true,"collect_contacts":false}});
    let mut config: crate::config::Config = serde_json::from_value(config)?;
    config.validate()?;
    let path = runtime.args.evidence.join(name);
    io::write(&path, toml::to_string(&config)?.as_bytes())?;
    Ok(path)
}

pub(super) async fn seed(runtime: &Runtime, state: &str, school: u32) -> Result<i64> {
    let url = format!(
        "https://{}.milesplit.com/teams/{school}-owned-workflow-fixture",
        state.to_lowercase()
    );
    let target = json!({"source":"milesplit","kind":"team","url":url,"context":{"state":state,"school_id":school.to_string(),"season":2026}});
    let key = io::digest(&serde_json::to_vec(&target)?);
    Ok(sqlx::query_scalar("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) VALUES('milesplit',$1,$2,$3,'failed',3,'unsupported fixture format') RETURNING id").bind(key).bind(url).bind(target).fetch_one(&runtime.pool).await?)
}

pub(super) async fn continuation(runtime: &Runtime, key: &str) -> Result<Value> {
    let until = io::deadline(Duration::from_secs(90))?;
    stream::iter(0..900)
        .map(Ok::<_, anyhow::Error>)
        .try_fold(None, |found, _| async move {
            if found.is_some() {
                return Ok(found);
            }
            ensure!(
                tokio::time::Instant::now() < until,
                "continuation did not finish within90seconds"
            );
            let value = call(runtime, &["status", "--key", key]).await?;
            if value.is_object() {
                return Ok(Some(value));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(None)
        })
        .await?
        .context("continuation did not complete")
}

pub(super) fn progress(value: &Value, selected: u64, rejected: u64) -> Result<()> {
    ensure!(
        value.pointer("/progress/selected").and_then(Value::as_u64) == Some(selected)
            && value.pointer("/progress/rejected").and_then(Value::as_u64) == Some(rejected),
        "workflow progress differs from exact fixture counts"
    );
    Ok(())
}

async fn prove_export(runtime: &Runtime, path: &str, identity: &str) -> Result<Value> {
    let operation = format!("cli-export-{identity}");
    let first_export = call(
        runtime,
        &["export", "--config", path, "--operation-id", &operation],
    )
    .await?;
    let duplicate_export = call(
        runtime,
        &["export", "--config", path, "--operation-id", &operation],
    )
    .await?;
    let export_id = io::invocation(&first_export)?;
    ensure!(
        export_id == io::invocation(&duplicate_export)?,
        "native export CLI failed deduplication"
    );
    let exported = runtime
        .http
        .output(&runtime.ingress, export_id, "receipt")
        .await?;
    io::artifact(
        exported
            .get("receipt")
            .context("native export receipt missing")?,
        &runtime.root,
    )?;
    Ok(json!({"invocation_id":export_id,"duplicate":duplicate_export,"result":exported}))
}

pub(super) async fn prove(runtime: &Runtime, identity: &str) -> Result<Value> {
    let path = config(runtime, "MI", 1, 1, "empty-focused-config.toml")?;
    let path = path.to_str().context("config path notUTF8")?;
    let export = prove_export(runtime, path, identity).await?;
    let legacy = seed(runtime, "MI", 942_615).await?;
    let generation = format!("cli-{identity}");
    let first = call(
        runtime,
        &[
            "crawl",
            "--config",
            path,
            "--generation",
            &generation,
            "--hours",
            "1",
            "--max-batches",
            "2",
        ],
    )
    .await?;
    let id = io::text(&first, "/submission/invocationId")?;
    let request = runtime.args.evidence.join("workflow-cli-request.json");
    io::save(
        &request,
        first.get("request").context("native CLI request missing")?,
    )?;
    let duplicate = call(
        runtime,
        &[
            "submit",
            "--request",
            request.to_str().context("request path notUTF8")?,
        ],
    )
    .await?;
    ensure!(
        io::invocation(&duplicate)? == id,
        "workflow CLI submit changed invocation"
    );
    let report = runtime
        .http
        .output(&runtime.ingress, id, "progress")
        .await?;
    progress(&report, 1, 1)?;
    let next_key = format!("{generation}-000001");
    ensure!(
        io::text(&report, "/continuation")? == next_key
            && report.get("stopped_reason") == Some(&Value::Null),
        "first native workflow did not schedule next batch"
    );
    let next = continuation(runtime, &next_key).await?;
    progress(&next, 0, 0)?;
    ensure!(
        next.get("continuation") == Some(&Value::Null)
            && io::text(&next, "/stopped_reason")? == BOUNDED,
        "native continuation exceeded batch bound"
    );
    ensure!(
        call(
            runtime,
            &["status", "--key", io::text(&first, "/workflow_key")?]
        )
        .await?
            == report,
        "native status differs from completed report"
    );
    let wires: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_wire_requests")
        .fetch_one(&runtime.pool)
        .await?;
    ensure!(wires == 0, "empty focused workflow attempted source HTTP");
    let checkpoint:Value=sqlx::query_scalar("SELECT jsonb_build_object('attempts',p.attempts,'status',p.status,'last_error',p.last_error,'operation',d.state) FROM scrape_pages p JOIN durable_page_operations d ON d.page_id=p.id WHERE p.id=$1").bind(legacy).fetch_one(&runtime.pool).await?;
    ensure!(
        checkpoint
            == json!({"attempts":3,"status":"failed","last_error":"unsupported fixture format","operation":"rejected"}),
        "native continuation rewrote legacy checkpoint"
    );
    let proof = json!({"invocation_id":id,"first":first,"duplicate":duplicate,"report":report,"native_start_submit_and_status":"passed","public_source_requests":wires,"continuation":next,"continuation_key":next_key,"legacy_checkpoint":checkpoint,"service_export_invocation":io::text(&export,"/invocation_id")?,"duplicate_service_export":export.get("duplicate"),"service_export":export.get("result")});
    io::save(
        &runtime.args.evidence.join("workflow-cli-proof.json"),
        &proof,
    )?;
    Ok(proof)
}

pub(super) async fn restored(runtime: &Runtime, proof: &Value) -> Result<Value> {
    let root = call(
        runtime,
        &["status", "--key", io::text(proof, "/first/workflow_key")?],
    )
    .await?;
    let next = call(
        runtime,
        &["status", "--key", io::text(proof, "/continuation_key")?],
    )
    .await?;
    ensure!(
        Some(&root) == proof.get("report") && Some(&next) == proof.get("continuation"),
        "workflow reports changed after backup restoration"
    );
    Ok(json!({"workflow_report_unchanged":true,"continuation_report_unchanged":true}))
}
