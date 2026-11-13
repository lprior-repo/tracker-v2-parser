use std::{fs, os::unix::fs::DirBuilderExt};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;

use crate::native_ops::recovery::{io, runtime::Runtime};

#[tracing::instrument(skip_all)]
pub(super) async fn prove(previous: &Runtime, identity: &str) -> Result<Value> {
    let directory = previous.args.evidence.join("policy-progress");
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut args = previous.args.clone();
    args.evidence = directory.clone();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&previous.database)
        .await?;
    let mut runtime = Runtime::new(args, pool, previous.database.clone(), identity)?;
    let result = async {
        runtime.start_server().await?;
        runtime.start_worker().await?;
        runtime.register_named("registration.json").await?;
        super::scenarios(&mut runtime, identity).await
    }
    .await;
    let cleanup = runtime.cleanup().await;
    ensure!(
        !previous.pool.is_closed(),
        "policy proof cleanup closed its parent pool"
    );
    io::save(&directory.join("cleanup.json"), &cleanup)?;
    ensure!(
        cleanup
            .as_object()
            .context("policy proof cleanup absent")?
            .values()
            .all(|value| value == &Value::Bool(true)),
        "policy proof owned process cleanup failed"
    );
    let mut proof = result?;
    let fields = proof.as_object_mut().context("policy proof absent")?;
    fields.insert("owned_cleanup".into(), cleanup);
    fields.insert("separate_restart_namespace".into(), json!(directory));
    io::save(&directory.join("proof.json"), &proof)?;
    Ok(proof)
}
