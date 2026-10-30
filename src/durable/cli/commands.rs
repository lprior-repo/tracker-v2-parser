use super::{RunOverrides, send};
use crate::config::{Config, RunSettings};
use crate::durable::{
    model::{CrawlPhase, CrawlRequest},
    operations::OperationRequest,
};
use anyhow::{Context, Result};
use std::path::Path;

pub(super) async fn start_crawl(
    config: &Path,
    generation: String,
    run: RunOverrides,
    ingress: &str,
) -> Result<()> {
    let value = crawl_request(
        Config::load(config)?,
        generation,
        run,
        chrono::Utc::now().timestamp_millis(),
    )?;
    let key = value.workflow_key();
    let response = send(
        ingress,
        &format!("restate/send/AthleticCrawl/{key}/run"),
        None,
        Some(&value),
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"workflow_key":key,"request":value,"submission":response})
        )?
    );
    Ok(())
}

pub(super) fn crawl_request(
    mut config: Config,
    generation: String,
    overrides: RunOverrides,
    now_ms: i64,
) -> Result<CrawlRequest> {
    let defaults = RunSettings::default();
    let settings = config.run.map_or(defaults, std::convert::identity);
    let run = RunSettings {
        hours: overrides
            .hours
            .map_or(settings.hours, std::convert::identity),
        max_batches: overrides
            .max_batches
            .map_or(settings.max_batches, std::convert::identity),
        cooldown_seconds: overrides
            .cooldown_seconds
            .map_or(settings.cooldown_seconds, std::convert::identity),
    };
    run.validate()?;
    config.run = Some(run);
    let value = CrawlRequest {
        generation,
        config,
        deadline_ms: now_ms
            .checked_add(
                i64::from(run.hours)
                    .checked_mul(3_600_000)
                    .context("crawl duration overflow")?,
            )
            .context("crawl deadline overflow")?,
        cooldown_ms: i64::from(run.cooldown_seconds)
            .checked_mul(1_000)
            .context("crawl cooldown overflow")?,
        batch: 0,
        max_batches: run.max_batches,
        phase: CrawlPhase::SchoolChains,
    };
    value.validate(now_ms)?;
    Ok(value)
}

pub async fn submit_operation(ingress: &str, value: OperationRequest) -> Result<()> {
    crate::durable::model::identifier(&value.operation_id)?;
    let key = format!(
        "{}-{}",
        value.operation_id,
        crate::durable::model::digest(&value.spec)?
    );
    let response = send(
        ingress,
        "restate/send/AthleticOperations/execute",
        Some(&key),
        Some(&value),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
