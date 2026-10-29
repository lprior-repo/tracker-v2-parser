use super::send;
use crate::durable::{
    model::{CrawlPhase, CrawlRequest},
    operations::OperationRequest,
};
use anyhow::{Context, Result};
use std::path::Path;

pub async fn start_crawl(
    config: &Path,
    generation: String,
    hours: u32,
    max_batches: u32,
    ingress: &str,
) -> Result<()> {
    let value = CrawlRequest {
        generation,
        config: crate::config::Config::load(config)?,
        deadline_ms: chrono::Utc::now()
            .timestamp_millis()
            .checked_add(
                i64::from(hours)
                    .checked_mul(3_600_000)
                    .context("crawl duration overflow")?,
            )
            .context("crawl deadline overflow")?,
        cooldown_ms: 60_000,
        batch: 0,
        max_batches,
        phase: CrawlPhase::SchoolChains,
    };
    value.validate(chrono::Utc::now().timestamp_millis())?;
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
