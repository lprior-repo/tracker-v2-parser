#[path = "export_transfer/projection.rs"]
mod projection;

use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use futures::{TryStreamExt, stream};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use track_scrapper_v2::{database::Database, model::AthleteRecord};

fn checksum(report: &serde_json::Value) -> Result<&str> {
    report
        .get("projected_sha256")
        .and_then(serde_json::Value::as_str)
        .context("missing projected digest")
}

#[derive(Parser)]
struct Arguments {
    #[arg(long, value_delimiter = ',')]
    states: Vec<String>,
    #[arg(long, default_value_t = 256)]
    samples: i64,
    #[arg(long)]
    output: PathBuf,
}

fn query(projected: bool) -> String {
    let records = include_str!("../src/database/athlete_records.sql")
        .replacen(
            "WHERE p.school_id IS NOT NULL",
            "WHERE p.athlete_id = $3::bigint AND p.school_id IS NOT NULL",
            1,
        )
        .replacen(
            "WHERE a.name IS NOT NULL",
            "WHERE a.id = $3::bigint AND a.name IS NOT NULL",
            1,
        );
    if projected {
        include_str!("../src/database/export_snapshot/workbook.sql")
            .replace("__RECORDS__", &records)
    } else {
        format!(
            "SELECT CASE WHEN octet_length(document::text) <= $4::bigint THEN document::text END, octet_length(document::text)::bigint FROM ({records}) report(document)"
        )
    }
}

async fn measure(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[i64],
    states: &[String],
    projected: bool,
) -> Result<serde_json::Value> {
    let statement = query(projected);
    let started = Instant::now();
    let (_, digest, bytes) = stream::iter(ids.iter().copied().map(Ok::<_, anyhow::Error>))
        .try_fold((tx, Sha256::new(), 0_u64), |(tx, mut digest, bytes), id| {
            let statement = &statement;
            async move {
                let (document, size): (Option<String>, i64) =
                    sqlx::query_as(sqlx::AssertSqlSafe(statement.as_str()))
                        .bind(states)
                        .bind([2027])
                        .bind(id)
                        .bind(8_388_608_i64)
                        .fetch_one(&mut **tx)
                        .await?;
                let document = document.context("sample exceeds canonical record byte limit")?;
                ensure!(size <= 8_388_608, "original size guard was bypassed");
                let bytes = bytes
                    .checked_add(u64::try_from(document.len())?)
                    .context("wire byte count overflow")?;
                let record: AthleteRecord = serde_json::from_str(&document)?;
                let encoded = serde_json::to_vec(&projection::record_for_sheet(record))?;
                digest.update(u64::try_from(encoded.len())?.to_be_bytes());
                digest.update(encoded);
                Ok((tx, digest, bytes))
            }
        })
        .await?;
    Ok(
        serde_json::json!({"seconds":started.elapsed().as_secs_f64(),"wire_bytes":bytes,
        "projected_sha256":format!("{:x}",digest.finalize()),"samples":ids.len()}),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(
        (1..=2048).contains(&arguments.samples),
        "sample bound must be1..2048"
    );
    ensure!(
        !arguments.output.exists(),
        "benchmark output already exists"
    );
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let database = Database::connect_for_export(&url).await?;
    let mut tx = database.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let ids: Vec<i64> =
        sqlx::query_scalar(include_str!("../src/database/export_snapshot_scope.sql"))
            .bind(&arguments.states)
            .bind([2027])
            .bind(0_i64)
            .bind(arguments.samples)
            .fetch_all(&mut *tx)
            .await?;
    ensure!(!ids.is_empty(), "benchmark cohort is empty");
    let first = measure(&mut tx, &ids, &arguments.states, false).await?;
    let projected = measure(&mut tx, &ids, &arguments.states, true).await?;
    let repeated = measure(&mut tx, &ids, &arguments.states, false).await?;
    ensure!(
        checksum(&first)? == checksum(&projected)? && checksum(&first)? == checksum(&repeated)?,
        "projected ordered records differ from the frozen Rust oracle"
    );
    tx.commit().await?;
    database.close().await;
    let report = serde_json::json!({"states":arguments.states,"baseline_cold":first,"projected":projected,"baseline_warm":repeated,"same_repeatable_read_snapshot":true,"production_mutations":false});
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
