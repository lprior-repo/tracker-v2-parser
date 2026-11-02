use std::{fs, os::unix::fs::DirBuilderExt};

use anyhow::{Context as _, Result, ensure};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;

use super::super::{io, runtime::Runtime};
use crate::durable::deadline_extension_proof::ExtensionWorker;

mod journal;
mod seed;
mod verify;
mod workflow;

pub(super) async fn prove(parent: &Runtime, identity: &str) -> Result<Value> {
    let evidence = parent.args.evidence.join("deadline-extension");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&evidence)?;
    fs::copy(
        parent.args.evidence.join("delivered.xlsx"),
        evidence.join("delivered.xlsx"),
    )
    .context("deadline extension workbook fixture copy failed")?;
    let mut args = parent.args.clone();
    args.evidence = evidence;
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&parent.database)
        .await?;
    let mut runtime = Runtime::new(
        args,
        pool,
        parent.database.clone(),
        &format!("{identity}-extension"),
    )?;
    let result = run(&mut runtime, identity).await;
    let cleanup = runtime.cleanup().await;
    io::save(&runtime.args.evidence.join("cleanup.json"), &cleanup)?;
    ensure!(
        cleanup
            .as_object()
            .context("deadline extension cleanup absent")?
            .values()
            .all(|value| value == &Value::Bool(true)),
        "deadline extension owned process cleanup failed"
    );
    let mut proof = result?;
    proof
        .as_object_mut()
        .context("deadline extension proof must be object")?
        .insert("owned_cleanup".into(), cleanup);
    proof
        .as_object_mut()
        .context("deadline extension proof must be object")?
        .insert("isolated_restate_runtime".into(), Value::Bool(true));
    io::save(
        &runtime
            .args
            .evidence
            .join("deadline-extension-restart-proof.json"),
        &proof,
    )?;
    Ok(proof)
}

async fn run(runtime: &mut Runtime, identity: &str) -> Result<Value> {
    runtime.start_server().await?;
    runtime.start_worker().await?;
    runtime
        .register_named("extension-worker-registration.json")
        .await?;
    let seeded = seed::prepare(runtime, identity).await?;
    let fixture = ExtensionWorker::start(&runtime.database, seeded.page_id).await?;
    let result = workflow::run(runtime, &seeded, fixture.endpoint()).await;
    fixture.stop().await?;
    result
}
