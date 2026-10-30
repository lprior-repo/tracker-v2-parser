mod backup;
mod capacity;
mod cli;
mod control;
mod database;
mod exercise;
mod fixtures;
mod io;
mod journal;
mod pipeline;
mod process;
mod review;
mod runtime;
mod workbook;

#[cfg(test)]
mod tests;

use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use clap::Args;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;

struct Execution {
    proof: Result<Value>,
    cleanup: Value,
}

#[derive(Debug, Args)]
pub struct RecoveryArgs {
    #[arg(long)]
    pub server: PathBuf,
    #[arg(long)]
    pub worker: PathBuf,
    #[arg(long)]
    pub evidence: PathBuf,
}

impl RecoveryArgs {
    fn validate(mut self) -> Result<Self> {
        for path in [&mut self.server, &mut self.worker] {
            *path = path
                .canonicalize()
                .context("native proof executable absent")?;
            let metadata = fs::metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
                "native proof requires executable regular files"
            );
        }
        self.evidence = std::path::absolute(&self.evidence)?;
        ensure!(
            !self.evidence.try_exists()? && fs::symlink_metadata(&self.evidence).is_err(),
            "proof evidence directory must be new"
        );
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.evidence)?;
        self.evidence = self.evidence.canonicalize()?;
        Ok(self)
    }
}

fn scoped_url(original: &str, schema: &str) -> Result<String> {
    let mut url = url::Url::parse(original)
        .map_err(|_| anyhow::anyhow!("TEST_DATABASE_URL must be a valid PostgreSQL URL"))?;
    ensure!(
        matches!(url.scheme(), "postgres" | "postgresql"),
        "TEST_DATABASE_URL must be PostgreSQL"
    );
    let query = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(query)
        .append_pair("options", &format!("-csearch_path={schema}"));
    Ok(url.into())
}

#[tracing::instrument(skip_all)]
pub async fn run(args: RecoveryArgs) -> Result<()> {
    let args = args.validate()?;
    let original = std::env::var("TEST_DATABASE_URL")
        .context("TEST_DATABASE_URL required; production fallback forbidden")?;
    let identity = io::digest(
        format!(
            "{}:{}",
            std::process::id(),
            chrono::Utc::now()
                .timestamp_nanos_opt()
                .context("proof clock out of range")?
        )
        .as_bytes(),
    );
    let identity = identity.get(..32).context("proof identity too short")?;
    let schema = format!("restate_e2e_{identity}");
    let scoped = scoped_url(&original, &schema)?;
    let evidence = args.evidence.clone();
    let mut report = json!({"status":"failed","schema":schema,"started_utc":chrono::Utc::now().to_rfc3339(),"binary_sha256":{"server":io::file_digest(&args.server,crate::export::MAX_EXPORT_BYTES)?.1,"worker":io::file_digest(&args.worker,crate::export::MAX_EXPORT_BYTES)?.1,"native_ops":io::file_digest(&std::env::current_exe()?,crate::export::MAX_EXPORT_BYTES)?.1},"scope":"isolated PostgreSQL schema, owned native processes, local source fixtures, delivery copies","not_proven":["mid-chunk process death; covered by native export recovery tests","provider outage recovery; scenario makes no provider HTTP requests"]});
    ensure!(
        schema.starts_with("restate_e2e_")
            && schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "proof schema identifier invalid"
    );
    let base = match database::setup(&original, &schema).await {
        Ok(pool) => pool,
        Err(failure) => {
            return finish_report(
                &mut report,
                Err(failure.error),
                &evidence,
                &original,
                &scoped,
                failure.schema_removed,
            );
        }
    };
    let outcome = execute(args, &scoped, identity).await;
    let removed = database::remove_schema(&base, &schema).await;
    base.close().await;
    if let Err(error) = &removed {
        report
            .as_object_mut()
            .context("proof report must be object")?
            .insert(
                "schema_cleanup_error".into(),
                json!(safe_error(error, &original, &scoped)),
            );
    }
    finish_report(
        &mut report,
        outcome,
        &evidence,
        &original,
        &scoped,
        removed.is_ok(),
    )
}

fn finish_report(
    report: &mut Value,
    outcome: Result<Execution>,
    evidence: &std::path::Path,
    original: &str,
    scoped: &str,
    dropped: bool,
) -> Result<()> {
    let object = report
        .as_object_mut()
        .context("proof report must be object")?;
    let settings = evidence.join("service-settings.json");
    if settings.try_exists()? {
        object.insert(
            "service_settings".into(),
            serde_json::from_slice(&io::read(&settings, io::JSON_LIMIT)?)?,
        );
    }
    match outcome {
        Ok(Execution { proof, mut cleanup }) => {
            cleanup
                .as_object_mut()
                .context("cleanup must be object")?
                .insert("schema_removed".into(), json!(dropped));
            let passed = proof.is_ok()
                && dropped
                && cleanup
                    .as_object()
                    .context("cleanup must be object")?
                    .values()
                    .all(|value| value == &Value::Bool(true));
            object.insert(
                "status".into(),
                json!(if passed { "passed" } else { "failed" }),
            );
            match proof {
                Ok(proof) => {
                    object.insert("proof".into(), proof);
                }
                Err(error) => {
                    object.insert(
                        "error".into(),
                        json!({"message":safe_error(&error,original,scoped)}),
                    );
                }
            }
            object.insert("cleanup".into(), cleanup);
        }
        Err(error) => {
            object.insert(
                "error".into(),
                json!({"message":safe_error(&error,original,scoped)}),
            );
            object.insert("cleanup".into(), json!({"schema_removed":dropped}));
        }
    }
    io::save(&evidence.join("report.json"), report)?;
    println!(
        "{}",
        json!({"status":report.get("status"),"report":evidence.join("report.json")})
    );
    ensure!(
        report.get("status") == Some(&json!("passed")),
        "native recovery proof failed; private evidence retained"
    );
    Ok(())
}

fn safe_error(error: &anyhow::Error, original: &str, scoped: &str) -> String {
    let mut text = format!("{error:#}")
        .replace(original, "[REDACTED]")
        .replace(scoped, "[REDACTED]");
    if let Ok(url) = url::Url::parse(original) {
        for secret in [Some(url.username()), url.password()]
            .into_iter()
            .flatten()
            .filter(|value| !value.is_empty())
        {
            text = text.replace(secret, "[REDACTED]");
        }
    }
    text.chars().take(2048).collect()
}

async fn execute(args: RecoveryArgs, scoped: &str, identity: &str) -> Result<Execution> {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(scoped)
        .await?;
    let mut runtime = runtime::Runtime::new(args, pool, scoped.to_owned(), identity)?;
    let result = async {
        runtime.start_server().await?;
        runtime.start_worker().await?;
        runtime.register().await?;
        let settings = runtime.settings().await?;
        io::save(
            &runtime.args.evidence.join("service-settings.json"),
            &settings,
        )?;
        exercise::run(&mut runtime, identity).await
    }
    .await;
    let cleanup = runtime.cleanup().await;
    io::save(&runtime.args.evidence.join("cleanup.json"), &cleanup)?;
    Ok(Execution {
        proof: result,
        cleanup,
    })
}
