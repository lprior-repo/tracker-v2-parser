use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{io, runtime::Runtime};

#[cfg(test)]
mod tests;

pub(super) async fn read(runtime: &Runtime, id: &str, label: &str) -> Result<Vec<Value>> {
    ensure!(
        id.starts_with("inv_")
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid native invocation ID"
    );
    runtime
        .query(
            &format!("SELECT * FROM sys_journal WHERE id='{id}' ORDER BY index"),
            label,
        )
        .await
}

fn entry(row: &Value) -> Result<Value> {
    let value = row.get("entry_json").context("journal entry JSON absent")?;
    match value {
        Value::String(value) => Ok(serde_json::from_str(value)?),
        _ => Ok(value.clone()),
    }
}

fn matching(row: &Value, children: &[String]) -> Result<Vec<String>> {
    let expression = regex::Regex::new("inv_[A-Za-z0-9]+")?;
    let body = serde_json::to_string(&entry(row)?)?;
    let ids = expression
        .find_iter(&body)
        .map(|matched| matched.as_str())
        .collect::<BTreeSet<_>>();
    Ok(children
        .iter()
        .filter(|id| ids.contains(id.as_str()))
        .cloned()
        .collect())
}

fn index(row: &Value) -> Result<u64> {
    row.get("index")
        .and_then(Value::as_u64)
        .context("journal index absent")
}

pub(super) fn validate_root(rows: &[Value], children: &[String], count: usize) -> Result<Value> {
    ensure!(children.len() == count, "pipeline child count mismatch");
    let mut ordered = rows.to_vec();
    ordered.sort_by_key(|row| row.get("index").and_then(Value::as_u64));
    let names = ordered
        .iter()
        .filter(|row| row.get("entry_type").and_then(Value::as_str) == Some("Command: Run"))
        .filter_map(|row| row.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    (0..count).try_for_each(|number| -> Result<()> {
        ["select-page-", "reserve-page-"]
            .into_iter()
            .try_for_each(|prefix| -> Result<()> {
                ensure!(
                    names
                        .iter()
                        .filter(|name| **name == format!("{prefix}{number:06}"))
                        .count()
                        == 1,
                    "pipeline must journal each select/reserve exactly once"
                );
                Ok(())
            })
    })?;
    let sends = ordered
        .iter()
        .filter(|row| {
            row.get("entry_type")
                .and_then(Value::as_str)
                .is_some_and(|name| name.to_lowercase().contains("onewaycall"))
        })
        .map(|row| Ok((row, serde_json::to_string(&entry(row)?)?)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, body)| body.contains("AthleticPage"))
        .map(|(row, _)| row)
        .collect::<Vec<_>>();
    ensure!(
        sends.len() == count,
        "pipeline must eagerly send each child exactly once"
    );
    let sent = sends
        .iter()
        .map(|row| -> Result<String> {
            let matches = matching(row, children)?;
            ensure!(
                matches.len() == 1,
                "send journal cannot identify exact child"
            );
            matches.into_iter().next().context("send child absent")
        })
        .collect::<Result<Vec<_>>>()?;
    let attached = ordered
        .iter()
        .filter(|row| {
            row.get("entry_type")
                .and_then(Value::as_str)
                .is_some_and(|name| name.to_lowercase().contains("attachinvocation"))
        })
        .map(|row| Ok((index(row)?, matching(row, children)?)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, ids)| ids.len() == 1)
        .map(|(index, ids)| {
            Ok((
                index,
                ids.into_iter().next().context("attach child absent")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        sent == children && attached.iter().map(|(_, id)| id).eq(children),
        "pipeline sends/ordered attaches differ from exact children"
    );
    sends
        .chunks(3)
        .zip(attached.chunks(3))
        .try_for_each(|(sends, attaches)| -> Result<()> {
            let last = sends
                .iter()
                .map(|row| index(row))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .max()
                .context("empty send window")?;
            let first = attaches
                .iter()
                .map(|(index, _)| *index)
                .min()
                .context("empty attach window")?;
            ensure!(
                last < first,
                "pipeline attached before all window sends were journaled"
            );
            Ok(())
        })?;
    Ok(
        json!({"selected":count,"sent_child_ids":sent,"ordered_attached_child_ids":attached.into_iter().map(|(_,id)|id).collect::<Vec<_>>(),"bounded_window":3,"window_sends_precede_attaches":true}),
    )
}

pub(super) async fn child(runtime: &Runtime, child: &Value, page: i64) -> Result<Value> {
    let id = io::text(child, "/id")?;
    let rows = read(runtime, id, &format!("pipeline-child-{page}")).await?;
    let inputs = rows
        .iter()
        .filter(|row| row.get("entry_type").and_then(Value::as_str) == Some("Command: Input"))
        .collect::<Vec<_>>();
    ensure!(
        inputs.len() == 1,
        "pipeline child must retain exactly one input"
    );
    let input = entry(inputs.first().context("child input absent")?)?;
    let bytes = io::array(&input, "/Command/Input/payload")?
        .iter()
        .map(|value| {
            u8::try_from(value.as_u64().context("input byte invalid")?).map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(bytes.len() <= 1_048_576, "child input exceeds1MiB");
    let execution: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        execution
            .pointer("/request/binding/page_id")
            .and_then(Value::as_i64)
            == Some(page)
            && execution
                .pointer("/config/limits/concurrency")
                .and_then(Value::as_u64)
                == Some(3),
        "child input differs from reserved page"
    );
    let result = runtime.http.output(&runtime.ingress, id, "state").await?;
    ensure!(
        io::text(&result, "/state")? == "rejected",
        "settled pipeline child must return rejected"
    );
    Ok(json!({"invocation_id":id,"page_id":page,"outcome":result}))
}

pub(super) async fn export(runtime: &Runtime, id: &str) -> Result<Value> {
    let rows = read(runtime, id, "journal").await?;
    runtime
        .query(
            &format!("SELECT * FROM sys_invocation WHERE id='{id}'"),
            "invocation",
        )
        .await?;
    let body = serde_json::to_string(&rows)?;
    let names = [
        "freeze-export-input",
        "read-frozen-chunk-plan",
        "prepare-athlete-chunk-000000",
        "assemble-workbook",
        "verify-workbook",
        "publish-verified-workbook",
        "deliver-verified-workbook",
        "complete-export-receipt",
    ];
    ensure!(
        names.iter().all(|name| body.contains(name)),
        "retained export journal lost required stages"
    );
    Ok(
        json!({"invocation_query_status":200,"journal_query_status":200,"retained_stage_names":names,"query_proof":"verified"}),
    )
}
