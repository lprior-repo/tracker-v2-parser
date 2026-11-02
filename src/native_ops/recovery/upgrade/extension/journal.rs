use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::super::super::runtime::Runtime;
use super::super::evidence;

pub(super) fn selected_run_output_contains(rows: &[Value], expected: &Value) -> Result<bool> {
    let entries = rows
        .iter()
        .map(evidence::decode)
        .collect::<Result<Vec<_>>>()?;
    let command = entries
        .iter()
        .find(|entry| {
            entry.pointer("/Command/Run/name").and_then(Value::as_str) == Some("select-page-000000")
        })
        .context("recorded page selection command absent")?;
    let completion_id = command
        .pointer("/Command/Run/completion_id")
        .and_then(Value::as_u64)
        .context("selection completion ID absent")?;
    let completion = entries
        .iter()
        .find(|entry| {
            entry
                .pointer("/Notification/Completion/Run/completion_id")
                .and_then(Value::as_u64)
                == Some(completion_id)
        })
        .context("recorded page selection completion absent")?;
    Ok(contains_json_value(completion, expected))
}

fn contains_json_value(haystack: &Value, expected: &Value) -> bool {
    if haystack == expected {
        return true;
    }
    match haystack {
        Value::Object(fields) => fields
            .values()
            .any(|value| contains_json_value(value, expected)),
        Value::Array(values) => {
            let bytes = values
                .iter()
                .map(|value| value.as_u64().and_then(|number| u8::try_from(number).ok()))
                .collect::<Option<Vec<_>>>();
            let decoded = bytes
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|value| serde_json::from_str::<Value>(&value).ok());
            decoded
                .as_ref()
                .is_some_and(|value| contains_json_value(value, expected))
                || values
                    .iter()
                    .any(|value| contains_json_value(value, expected))
        }
        _ => false,
    }
}

pub(super) async fn wait_for_sleep(runtime: &Runtime, id: &str) -> Result<()> {
    evidence::identifier(id, "inv_")?;
    let rows = evidence::wait(
        runtime,
        &format!("SELECT index,entry_json FROM sys_journal WHERE id='{id}' ORDER BY index"),
        |rows| {
            let Ok(entries) = rows
                .iter()
                .map(evidence::decode)
                .collect::<Result<Vec<_>>>()
            else {
                return false;
            };
            entries
                .iter()
                .any(|entry| entry.pointer("/Command/Sleep").is_some())
                && entries
                    .iter()
                    .all(|entry| entry.pointer("/Notification/Completion/Sleep").is_none())
        },
    )
    .await?;
    let entries = rows
        .iter()
        .map(evidence::decode)
        .collect::<Result<Vec<_>>>()?;
    let sleep = entries
        .iter()
        .find_map(|entry| entry.pointer("/Command/Sleep"))
        .context("pending deadline extension timer absent")?;
    ensure!(
        sleep
            .get("wake_up_time")
            .and_then(Value::as_i64)
            .is_some_and(|wake| wake > chrono::Utc::now().timestamp_millis()),
        "deadline extension fixture timer already elapsed before restart"
    );
    Ok(())
}
