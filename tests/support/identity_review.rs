use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::database::Database;

use super::support::{Scenario, assert_success};

pub const PROFILES: [(&str, &str); 2] = [
    ("14441466", "truman-leferve"),
    ("17866032", "truman-lefevre"),
];

pub fn url(index: usize) -> String {
    format!(
        "https://wi.milesplit.com/athletes/{}-{}",
        PROFILES[index].0, PROFILES[index].1
    )
}

pub fn config(test: &Scenario) -> Result<PathBuf> {
    let path = test.directory().join("identity-config.toml");
    super::support::write_config(&path)?;
    let text = std::fs::read_to_string(&path)?;
    std::fs::write(
        &path,
        text.replace(
            "rows_per_file = 1000",
            "rows_per_file = 1000\nlayout = 'single_sheet'",
        ),
    )?;
    Ok(path)
}

pub async fn replay(test: &Scenario, index: usize, section: &str) -> Result<()> {
    let config = config(test)?;
    let input = super::support::fixture(&format!(
        "identity/truman-{}-{section}.html",
        PROFILES[index].0
    ));
    let target = if section == "base" {
        url(index)
    } else {
        format!("{}/progression", url(index))
    };
    let output = test
        .cli(&[
            "scrape",
            "--config",
            config.to_str().context("config path")?,
            "--input",
            input.to_str().context("fixture path")?,
            "--source",
            "milesplit",
            "--kind",
            "athlete",
            "--url",
            &target,
            "--state",
            "WI",
            "--season",
            "2026",
            "--athlete-id",
            PROFILES[index].0,
        ])
        .await?;
    assert_success(&output);
    Ok(())
}

pub async fn seed(test: &Scenario) -> Result<()> {
    for index in 0..2 {
        replay(test, index, "base").await?;
        replay(test, index, "progression").await?;
    }
    let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
        .fetch_one(&test.pool)
        .await?;
    assert_eq!(results, 12);
    Ok(())
}

pub async fn manifest(test: &Scenario) -> Result<Value> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT source_id,athlete_id FROM athlete_sources ORDER BY id")
            .fetch_all(&test.pool)
            .await?;
    assert_eq!(rows.len(), 2);
    Ok(
        json!({"schema_revision":1,"canonical_athlete_id":rows.iter().map(|row|row.1).min(),
        "preferred_name":"Truman Lefevre","accepted_names":["Truman LeFerve","Truman Lefevre"],
        "reviewed_by":"user","reason":"Truman LeFerve / Truman Lefevre: that is same person",
        "profiles":rows.iter().map(|row|json!({"source":"milesplit","source_id":row.0,"expected_athlete_id":row.1})).collect::<Vec<_>>() }),
    )
}

pub fn write_manifest(test: &Scenario, manifest: &Value) -> Result<(PathBuf, String)> {
    let path = test.directory().join("reviewed-identity.json");
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    std::fs::write(&path, bytes)?;
    Ok((path, digest))
}

pub async fn apply(test: &Scenario, manifest: &Value) -> Result<std::process::Output> {
    let config = config(test)?;
    let (path, digest) = write_manifest(test, manifest)?;
    test.cli(&[
        "scrape",
        "--config",
        config.to_str().context("config path")?,
        "--athlete-identity-manifest",
        path.to_str().context("manifest path")?,
        "--athlete-identity-sha256",
        &digest,
    ])
    .await
}

pub async fn queue(test: &Scenario) -> Result<Value> {
    sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY id),'[]'::jsonb) FROM scrape_pages p",
    )
    .fetch_one(&test.pool)
    .await
    .map_err(Into::into)
}

pub async fn results(test: &Scenario) -> Result<Value> {
    sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(r)-'athlete_id'-'last_seen' ORDER BY id),'[]'::jsonb) FROM results r")
        .fetch_one(&test.pool).await.map_err(Into::into)
}

pub async fn profiles(test: &Scenario) -> Result<Value> {
    sqlx::query_scalar("SELECT COALESCE(jsonb_agg((to_jsonb(p)-'athlete_id'-'details') || jsonb_build_object('details',p.details-'_reviewed_identity') ORDER BY id),'[]'::jsonb) FROM athlete_sources p")
        .fetch_one(&test.pool).await.map_err(Into::into)
}

pub async fn canonical_count(test: &Scenario) -> Result<i64> {
    sqlx::query_scalar("SELECT count(DISTINCT athlete_id) FROM athlete_sources")
        .fetch_one(&test.pool)
        .await
        .map_err(Into::into)
}

pub async fn records(test: &Scenario) -> Result<Vec<track_scrapper_v2::model::AthleteRecord>> {
    let database = Database::connect(&test.url).await?;
    let result = database.athlete_records().await;
    database.close().await;
    result
}
