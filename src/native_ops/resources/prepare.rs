use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{ConfigurationProfile, configuration_plan, model::PrepareArgs, telemetry};

pub fn prepare_configuration(
    profile: ConfigurationProfile,
    host_gib: u16,
    stack_gib: u16,
    observed: &Value,
) -> Result<Value> {
    ensure!(
        observed.get("postgres_major").and_then(Value::as_u64) == Some(18),
        "configuration preparation requires PostgreSQL18"
    );
    ensure!(
        observed
            .get("transaction_read_only")
            .and_then(Value::as_str)
            == Some("on"),
        "configuration evidence must come from read-only snapshot"
    );
    ["fsync", "full_page_writes", "synchronous_commit"]
        .iter()
        .try_for_each(|name| {
            ensure!(
                observed
                    .pointer(&format!("/settings/{name}/value"))
                    .and_then(Value::as_str)
                    == Some("on"),
                "observed durability setting {name} must be on"
            );
            Ok::<_, anyhow::Error>(())
        })?;
    let plan = configuration_plan(profile, host_gib, stack_gib)?;
    let changes = plan
        .get("changes")
        .and_then(Value::as_object)
        .context("missing configuration changes")?;
    let mut lines = vec![
        "fsync = 'on'".to_owned(),
        "full_page_writes = 'on'".to_owned(),
        "synchronous_commit = 'on'".to_owned(),
    ];
    changes
        .iter()
        .filter(|(_, value)| value.is_string())
        .try_for_each(|(name, value)| {
            let value = value.as_str().context("setting value is not text")?;
            ensure!(
                safe_value(name) && safe_value(value),
                "unsupported configuration syntax"
            );
            lines.push(format!("{name} = '{value}'"));
            Ok::<_, anyhow::Error>(())
        })?;
    if profile == ConfigurationProfile::Observability {
        lines.push(preloads(observed)?);
    }
    let content = format!("{}\n", lines.join("\n"));
    let evidence = serde_json::to_vec(observed)?;
    ensure!(
        evidence.len() <= 256 * 1024,
        "configuration evidence exceeds256KiB"
    );
    let maintenance = changes.get("table_options").cloned();
    Ok(
        json!({"format":"native-postgres-prepared-v1","applied":false,"plan":plan,
        "observed_at":observed.get("observed_at"),"observed_sha256":format!("{:x}",Sha256::digest(&evidence)),
        "configuration":content,"configuration_sha256":format!("{:x}",Sha256::digest(content.as_bytes())),
        "table_options":maintenance,
        "installation_required":"qualified native handoff must validate backup/replay receipts, preserve existing settings, and explicitly install this bounded artifact before reload/restart; no settings were changed"}),
    )
}

fn safe_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.,/-$".contains(&byte))
}

fn preloads(observed: &Value) -> Result<String> {
    let previous = observed
        .pointer("/settings/shared_preload_libraries/value")
        .and_then(Value::as_str)
        .context("existing shared_preload_libraries must be observed")?;
    ensure!(previous.len() <= 4096, "existing preloads exceed4096bytes");
    let mut libraries=previous.split(',').map(str::trim).filter(|value|!value.is_empty()).map(|value|{
        ensure!(safe_value(value),"existing preload contains unsupported syntax; retain configuration for manual review");
        Ok(value)
    }).collect::<Result<Vec<_>>>()?;
    ensure!(libraries.len() <= 32, "too many existing preloads");
    if !libraries.contains(&"pg_stat_statements") {
        libraries.push("pg_stat_statements");
    }
    Ok(format!(
        "shared_preload_libraries = '{}'",
        libraries.join(",")
    ))
}

#[tracing::instrument(skip_all)]
pub(super) async fn run(args: PrepareArgs) -> Result<()> {
    let observed = telemetry::snapshot(&args.database).await?;
    validate_observation_age(&observed, chrono::Utc::now())?;
    let prepared = prepare_configuration(
        args.profile,
        args.host_memory_gib,
        args.stack_memory_gib,
        &observed,
    )?;
    super::staging::stage(&args, &prepared)
}

pub fn validate_observation_age(
    observed: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let captured = chrono::DateTime::parse_from_rfc3339(
        observed
            .get("observed_at")
            .and_then(Value::as_str)
            .context("observation timestamp absent")?,
    )?;
    let age = now
        .signed_duration_since(captured)
        .to_std()
        .context("configuration observation is future dated")?;
    ensure!(
        age <= std::time::Duration::from_mins(2),
        "configuration observation must be no more than120seconds old and not future dated"
    );
    Ok(())
}
