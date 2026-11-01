use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};

use super::super::{io, runtime::Runtime};

pub(super) async fn query(runtime: &Runtime, statement: &str) -> Result<Vec<Value>> {
    let (status, value) = runtime
        .http
        .request(
            &runtime.admin,
            "/query",
            Some(&json!({"query":statement})),
            None,
        )
        .await?;
    ensure!(
        status == 200,
        "owned upgrade introspection failed HTTP{status}"
    );
    Ok(io::array(&value, "/rows")?.clone())
}

pub(super) async fn journal(runtime: &Runtime, id: &str) -> Result<Vec<Value>> {
    identifier(id, "inv_")?;
    query(
        runtime,
        &format!("SELECT index,entry_json FROM sys_journal WHERE id='{id}' ORDER BY index"),
    )
    .await
}

pub(super) fn decode(row: &Value) -> Result<Value> {
    let value = row.get("entry_json").context("journal entry absent")?;
    match value {
        Value::String(text) => Ok(serde_json::from_str(text)?),
        _ => Ok(value.clone()),
    }
}

pub(super) fn input(rows: &[Value]) -> Result<(Vec<u8>, Value)> {
    let first = rows
        .iter()
        .find(|row| row.get("index").and_then(Value::as_u64) == Some(0))
        .context("retained input absent")?;
    let decoded = decode(first)?;
    let bytes = io::array(&decoded, "/Command/Input/payload")?
        .iter()
        .take(1_048_577)
        .map(|byte| {
            u8::try_from(byte.as_u64().context("invalid input byte")?).map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(bytes.len() <= 1_048_576, "retained input exceeds1MiB");
    let input = serde_json::from_slice(&bytes)?;
    Ok((bytes, input))
}

pub(super) fn identifier(value: &str, prefix: &str) -> Result<()> {
    ensure!(
        value.starts_with(prefix)
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid owned invocation or deployment identity"
    );
    Ok(())
}

pub(super) async fn wait<F>(runtime: &Runtime, statement: &str, predicate: F) -> Result<Vec<Value>>
where
    F: Fn(&[Value]) -> bool + Send + Sync,
{
    let deadline = io::deadline(Duration::from_mins(1))?;
    stream::iter(0..300)
        .map(Ok::<_, anyhow::Error>)
        .try_fold(None, |found, _| {
            let predicate = &predicate;
            async move {
                if found.is_some() {
                    return Ok(found);
                }
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "owned upgrade condition exceeded60seconds"
                );
                let rows = query(runtime, statement).await?;
                if predicate(&rows) {
                    return Ok(Some(rows));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(None)
            }
        })
        .await?
        .context("owned upgrade condition was not observed")
}

pub(super) async fn no_attempts(runtime: &Runtime) -> Result<Value> {
    let value:Value=sqlx::query_scalar("SELECT jsonb_build_object('acquisition_attempts',(SELECT count(*) FROM durable_page_attempts),'wire_requests',(SELECT count(*) FROM durable_wire_requests),'unfinished_page_operations',(SELECT count(*) FROM durable_page_operations WHERE state IN ('reserved','captured','parsed')))").fetch_one(&runtime.pool).await?;
    ensure!(
        value == json!({"acquisition_attempts":0,"wire_requests":0,"unfinished_page_operations":0}),
        "upgrade proof attempted source acquisition"
    );
    Ok(value)
}
