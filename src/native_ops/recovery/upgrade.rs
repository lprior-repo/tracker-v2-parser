mod evidence;
mod handoff;
mod rolling;

use std::{fs, os::unix::fs::DirBuilderExt};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;

use super::{RecoveryArgs, database, io, runtime::Runtime};

pub(super) async fn prove_if_requested(
    args: &RecoveryArgs,
    original: &str,
    identity: &str,
) -> Result<Value> {
    let Some(previous) = &args.previous_worker else {
        return Ok(json!({"requested":false}));
    };
    let previous_hash = io::file_digest(previous, crate::export::MAX_EXPORT_BYTES)?.1;
    let worker_hash = io::file_digest(&args.worker, crate::export::MAX_EXPORT_BYTES)?.1;
    ensure!(
        previous_hash != worker_hash,
        "upgrade proof requires different old and new immutable binaries"
    );
    let schema = format!("restate_e2e_upgrade_{identity}");
    let scoped = super::scoped_url(original, &schema)?;
    let base = database::setup(original, &schema)
        .await
        .map_err(|failure| failure.error)?;
    let directory = args.evidence.join("upgrade");
    let result = exercise(args, &directory, &scoped, identity).await;
    let removed = database::remove_schema(&base, &schema).await;
    base.close().await;
    removed?;
    let mut result = result?;
    ensure!(
        previous_hash == io::file_digest(previous, crate::export::MAX_EXPORT_BYTES)?.1
            && worker_hash == io::file_digest(&args.worker, crate::export::MAX_EXPORT_BYTES)?.1,
        "upgrade worker binary changed during qualification"
    );
    result
        .as_object_mut()
        .context("upgrade proof must be object")?
        .insert("owned_schema_removed".into(), json!(true));
    result
        .as_object_mut()
        .context("upgrade proof must be object")?
        .insert("status".into(), json!("passed"));
    result
        .as_object_mut()
        .context("upgrade proof must be object")?
        .insert(
            "binary_sha256".into(),
            json!({"previous_worker":previous_hash,"worker":worker_hash}),
        );
    io::save(&directory.join("proof.json"), &result)?;
    Ok(result)
}

async fn exercise(
    args: &RecoveryArgs,
    directory: &std::path::Path,
    scoped: &str,
    identity: &str,
) -> Result<Value> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let mut args = args.clone();
    args.evidence = directory.to_owned();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(scoped)
        .await?;
    let mut runtime = Runtime::new(args, pool, scoped.to_owned(), identity)?;
    let result = async {
        runtime.start_server().await?;
        runtime.start_previous_worker().await?;
        let previous = runtime.register_named("previous-registration.json").await?;
        let handoff = handoff::prove(&mut runtime, identity, &previous).await?;
        let rolling = rolling::prove(&mut runtime, identity).await?;
        Ok::<_, anyhow::Error>(
            json!({"requested":true,"handoff":handoff,"rolling_restart":rolling}),
        )
    }
    .await;
    let cleanup = runtime.cleanup().await;
    io::save(&directory.join("cleanup.json"), &cleanup)?;
    ensure!(
        cleanup
            .as_object()
            .context("upgrade cleanup absent")?
            .values()
            .all(|value| value == &Value::Bool(true)),
        "upgrade owned process cleanup failed"
    );
    let mut result = result?;
    result
        .as_object_mut()
        .context("upgrade proof must be object")?
        .insert("owned_cleanup".into(), cleanup);
    Ok(result)
}
