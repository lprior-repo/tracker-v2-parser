use std::time::Instant;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use futures::{StreamExt, TryStreamExt, stream};
use sha2::{Digest, Sha256};
use track_scrapper_v2::database::{Database, EXPORT_BATCH_ROWS, ExportSnapshot};

const MAX_BATCHES: usize = 10_000;

#[derive(Parser)]
struct Arguments {
    #[arg(long, value_delimiter = ',')]
    states: Vec<String>,
    #[arg(long, default_value_t = 2027)]
    year: i32,
}

struct Sample<'a> {
    snapshot: ExportSnapshot<'a>,
    digest: Sha256,
    rows: u64,
    bytes: u64,
    batches: u64,
    finished: bool,
}

impl Sample<'_> {
    #[tracing::instrument(skip(self))]
    async fn next(mut self) -> Result<Self> {
        if self.finished {
            return Ok(self);
        }
        let Some(records) = self.snapshot.next_records(EXPORT_BATCH_ROWS).await? else {
            self.finished = true;
            return Ok(self);
        };
        ensure!(!records.is_empty(), "snapshot returned an empty live batch");
        self.batches = self
            .batches
            .checked_add(1)
            .context("batch count overflow")?;
        records.iter().try_for_each(|record| -> Result<()> {
            let serialized = serde_json::to_vec(record)?;
            self.digest
                .update(u64::try_from(serialized.len())?.to_be_bytes());
            self.digest.update(&serialized);
            self.bytes = self
                .bytes
                .checked_add(u64::try_from(serialized.len())?)
                .context("snapshot byte count overflow")?;
            self.rows = self
                .rows
                .checked_add(1)
                .context("snapshot row count overflow")?;
            Ok(())
        })?;
        Ok(self)
    }
}

async fn measure(database: &Database, arguments: &Arguments) -> Result<serde_json::Value> {
    let started = Instant::now();
    let snapshot = database
        .begin_export_snapshot(&arguments.states, &[arguments.year])
        .await?;
    let initial = Sample {
        snapshot,
        digest: Sha256::new(),
        rows: 0,
        bytes: 0,
        batches: 0,
        finished: false,
    };
    let sample = stream::iter(0..MAX_BATCHES)
        .map(Ok::<_, anyhow::Error>)
        .try_fold(initial, |sample, _| sample.next())
        .await?;
    ensure!(sample.finished, "snapshot exceeded the static batch bound");
    let elapsed = started.elapsed().as_secs_f64();
    let report = serde_json::json!({
        "states": arguments.states,
        "year": arguments.year,
        "rows": sample.rows,
        "serialized_bytes": sample.bytes,
        "batches": sample.batches,
        "seconds": elapsed,
        "rows_per_second": f64::from(u32::try_from(sample.rows)?) / elapsed,
        "ordered_canonical_sha256": format!("{:x}", sample.digest.finalize()),
        "scope": "read-only canonical snapshot extraction, including bounded serialization and hashing; excludes workbook rendering and publication"
    });
    sample.snapshot.finish().await?;
    Ok(report)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let database = Database::connect_for_export(&url).await?;
    let report = measure(&database, &arguments).await?;
    database.close().await;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
