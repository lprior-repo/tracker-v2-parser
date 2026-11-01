use std::{io::Write, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};

use super::{SampleArgs, capacity::observe_capacity, delta::counter_delta};

const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;

#[tracing::instrument(skip_all)]
pub async fn snapshot(database: &str) -> Result<Value> {
    let mut connection = super::super::migration::database::connect(database).await?;
    let result = snapshot_connection(&mut connection).await;
    connection.close().await?;
    result
}

#[tracing::instrument(skip_all)]
pub async fn snapshot_connection(connection: &mut PgConnection) -> Result<Value> {
    let mut transaction = connection.begin().await?;
    sqlx::raw_sql("SET TRANSACTION READ ONLY; SET LOCAL statement_timeout='8s'; SET LOCAL lock_timeout='1s'; SET LOCAL idle_in_transaction_session_timeout='10s'; SET LOCAL stats_fetch_consistency='none'")
        .execute(&mut *transaction).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(9),
        sqlx::query_scalar::<_, Value>(include_str!("telemetry.sql")).fetch_one(&mut *transaction),
    )
    .await
    .context("resource snapshot deadline exceeded")?;
    transaction.rollback().await?;
    result.context("resource snapshot unavailable; connection details withheld")
}

#[tracing::instrument(skip_all)]
pub(super) async fn run(args: SampleArgs) -> Result<()> {
    args.validate()?;
    ensure!(
        !args.output.try_exists()?,
        "resource report must be a new file"
    );
    let database = args.database.as_str();
    let capacity_path = &args.capacity_path;
    let interval = args.interval_seconds;
    let collect = stream::iter(0..args.samples).map(Ok::<_,anyhow::Error>)
        .try_fold(Vec::with_capacity(usize::from(args.samples)), |mut rows,index| async move {
            if index > 0 { tokio::time::sleep(Duration::from_secs(u64::from(interval))).await; }
            let sample = snapshot(database).await?;
            ensure!(serde_json::to_vec(&sample)?.len() <= 256*1024,"individual resource snapshot exceeds 256 KiB");
            let host = super::host::observe().await?;
            rows.push(json!({"postgres":sample,"host":host,"capacity":observe_capacity(capacity_path)?}));
            Ok::<_,anyhow::Error>(rows)
        });
    let samples = tokio::time::timeout(Duration::from_mins(10), collect)
        .await
        .context("resource sampling exceeded 600 seconds")??;
    let deltas = samples
        .windows(2)
        .map(|pair| match (pair.first(), pair.get(1)) {
            (Some(first), Some(last)) => json!({"postgres":counter_delta(
                first.get("postgres").map_or(&Value::Null, |value| value),
                last.get("postgres").map_or(&Value::Null, |value| value),
            ),"host":super::interval::host_delta(
                first.get("host").map_or(&Value::Null,|value|value),
                last.get("host").map_or(&Value::Null,|value|value))}),
            _ => Value::Null,
        })
        .collect::<Vec<_>>();
    save(
        &args.output,
        &json!({"format":"native-resource-samples-v1","read_only":true,"samples":samples,"deltas":deltas}),
    )
}

pub(super) fn save(path: &Path, value: &Value) -> Result<()> {
    save_bytes(path, &serde_json::to_vec_pretty(value)?)
}

pub(super) fn save_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(path.is_absolute(), "resource report path must be absolute");
    ensure!(
        bytes.len() <= MAX_REPORT_BYTES,
        "resource report exceeds 16 MiB"
    );
    let parent = path
        .parent()
        .context("resource report needs parent directory")?;
    let temporary = tempfile::Builder::new()
        .prefix(".native-resources-")
        .tempfile_in(parent)?;
    temporary.as_file().write_all(bytes)?;
    temporary.as_file().sync_all()?;
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o600))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
