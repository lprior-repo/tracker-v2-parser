use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::native_ops::recovery::{io, runtime::Runtime};

async fn ledger(runtime: &Runtime, input: &Value) -> Result<Value> {
    let operation = io::text(input, "/request/binding/operation_id")?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('operation',to_jsonb(d),'attempt',to_jsonb(a),'checkpoint',to_jsonb(p)) FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id) JOIN scrape_pages p ON p.id=d.page_id WHERE d.operation_id=$1")
        .bind(operation).fetch_all(&runtime.pool).await?;
    ensure!(
        rows.len() == 1,
        "reserved recovery must retain exactly one original attempt"
    );
    rows.into_iter()
        .next()
        .context("reserved recovery ledger absent")
}

pub(super) async fn reserved(runtime: &Runtime, input: &Value) -> Result<Value> {
    let value = ledger(runtime, input).await?;
    ensure!(
        value.pointer("/attempt/state").and_then(Value::as_str) == Some("reserved")
            && value.pointer("/operation/state").and_then(Value::as_str) == Some("active")
            && value
                .pointer("/checkpoint/attempts")
                .and_then(Value::as_i64)
                == Some(1),
        "previous native worker did not preserve reserved attempt"
    );
    ensure!(
        value.pointer("/operation/binding") == input.pointer("/request/binding"),
        "original operation binding differs"
    );
    Ok(value)
}

pub(super) async fn completed(runtime: &Runtime, input: &Value, before: &Value) -> Result<Value> {
    let value = ledger(runtime, input).await?;
    ensure!(
        value.pointer("/attempt/state").and_then(Value::as_str) == Some("completed")
            && value.pointer("/operation/state").and_then(Value::as_str) == Some("completed"),
        "recovery did not complete existing reservation"
    );
    [
        "/operation/binding",
        "/operation/cycle",
        "/operation/cycle_attempts",
        "/operation/original_attempts",
        "/attempt/attempt_id",
        "/attempt/cycle",
        "/attempt/ordinal",
        "/attempt/total_attempts",
        "/checkpoint/attempts",
    ]
    .into_iter()
    .try_for_each(|path| -> Result<()> {
        ensure!(
            value.pointer(path) == before.pointer(path),
            "recovery changed ownership or attempts at {path}"
        );
        Ok(())
    })?;
    let wires: i64 =
        sqlx::query_scalar("SELECT count(*) FROM durable_wire_requests WHERE operation_id=$1")
            .bind(io::text(input, "/request/binding/operation_id")?)
            .fetch_one(&runtime.pool)
            .await?;
    ensure!(
        wires == 0,
        "offline reserved proof attempted a source request"
    );
    Ok(value)
}

pub(super) async fn checkpoints(runtime: &Runtime, ids: &[i64]) -> Result<Value> {
    super::super::handoff::checkpoints(runtime, ids).await
}

pub(super) async fn export(runtime: &Runtime, input: &Value, identity: &str) -> Result<Value> {
    let request = serde_json::json!({"operation_id":format!("local-stop-export-{identity}"),"spec":{"kind":"export","config":input.get("config").context("recovery export config absent")?}});
    let (id, _) = runtime.http.submit(&runtime.ingress, &request).await?;
    let result = runtime
        .http
        .output(&runtime.ingress, &id, "receipt")
        .await?;
    let receipt = io::artifact(
        result
            .get("receipt")
            .context("recovered export receipt absent")?,
        &runtime.root,
    )?;
    let retained: Value = serde_json::from_slice(&io::read(&receipt, io::JSON_LIMIT)?)?;
    let descriptor = retained
        .pointer("/publication/workbook")
        .context("recovered workbook descriptor absent")?;
    let path = io::artifact(descriptor, &runtime.root)?;
    tokio::task::spawn_blocking(move || -> Result<()> {
        use calamine::Reader;
        let mut book: calamine::Xlsx<_> = calamine::open_workbook(&path)?;
        let range = book.worksheet_range("Athletes")?;
        ensure!(
            range.height() <= 10 && range.width() <= 1000,
            "recovered fixture workbook exceeds bounds"
        );
        ensure!(
            range.rows().any(|row| row
                .iter()
                .any(|cell| matches!(cell,calamine::Data::String(value) if value=="Brice Fuller"))
                && row
                    .iter()
                    .any(|cell| cell.to_string().starts_with("4:12.69"))),
            "recovered reservation lost athlete or public PR in export"
        );
        Ok(())
    })
    .await
    .context("recovered workbook verification")??;
    Ok(
        serde_json::json!({"invocation_id":id,"workbook":descriptor,"recovered_athlete":"Brice Fuller","public_pr":"4:12.69","verified":true}),
    )
}
