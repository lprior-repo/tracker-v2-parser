use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::{Value, json};

use super::{cli, fixtures, io, journal, runtime::Runtime};

async fn checkpoints(runtime: &Runtime, ids: &[i64]) -> Result<Value> {
    ensure!(
        ids.len() == 7 && ids.iter().all(|id| *id > 0),
        "pipeline proof requires seven positive checkpoint IDs"
    );
    Ok(sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p WHERE id=ANY($1)",
    )
    .bind(ids)
    .fetch_one(&runtime.pool)
    .await?)
}

async fn check_domain(runtime: &Runtime, before: &Value) -> Result<()> {
    let mut expected = before.clone();
    for key in ["attempts", "seeded_legacy_attempts"] {
        let next = before
            .get(key)
            .and_then(Value::as_i64)
            .context("domain count absent")?
            .checked_add(21)
            .context("domain count overflow")?;
        expected
            .as_object_mut()
            .context("domain must be object")?
            .insert(key.into(), json!(next));
    }
    let after = fixtures::domain(runtime).await?;
    ensure!(
        after == expected && after.get("source_wire_requests").and_then(Value::as_u64) == Some(0),
        "pipeline changed domain facts or attempted source HTTP"
    );
    Ok(())
}

async fn children(
    runtime: &Runtime,
    first: &Value,
    next: &str,
    generation: &str,
    ids: &[i64],
) -> Result<(Vec<Value>, Vec<Value>)> {
    let children=runtime.query(&format!("SELECT id,idempotency_key,status FROM sys_invocation WHERE target_service_name='AthleticPage' AND idempotency_key LIKE '{generation}-%' ORDER BY idempotency_key"),"pipeline-children").await?;
    let first_key = io::text(first, "/workflow_key")?;
    let keys = (0..5)
        .map(|index| format!("{first_key}-page-{index:06}"))
        .chain((0..2).map(|index| format!("{next}-page-{index:06}")))
        .collect::<Vec<_>>();
    ensure!(
        children
            .iter()
            .map(|child| io::text(child, "/idempotency_key"))
            .collect::<Result<Vec<_>>>()?
            .iter()
            .copied()
            .eq(keys.iter().map(String::as_str)),
        "pipeline did not create exactly seven keyed children"
    );
    let child_ids = children
        .iter()
        .map(|child| io::text(child, "/id").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        child_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == 7,
        "pipeline child identities are not unique"
    );
    let evidence = stream::iter(children.iter().zip(ids).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut evidence, (child, id)| async move {
            evidence.push(journal::child(runtime, child, *id).await?);
            Ok(evidence)
        })
        .await?;
    let root_rows = journal::read(
        runtime,
        io::text(first, "/submission/invocationId")?,
        "pipeline-root-journal",
    )
    .await?;
    let continuation = runtime
        .query(
            &format!("SELECT id FROM sys_invocation WHERE target='AthleticCrawl/{next}/run'"),
            "pipeline-continuation-invocation",
        )
        .await?;
    ensure!(
        continuation.len() == 1,
        "pipeline continuation invocation must be unique"
    );
    let next_id = io::text(
        continuation.first().context("continuation ID absent")?,
        "/id",
    )?;
    let next_rows = journal::read(runtime, next_id, "pipeline-continuation-journal").await?;
    let root_ids = child_ids
        .get(..5)
        .context("first pipeline children absent")?;
    let next_ids = child_ids
        .get(5..)
        .context("next pipeline children absent")?;
    Ok((
        evidence,
        vec![
            journal::validate_root(&root_rows, root_ids, 5)?,
            journal::validate_root(&next_rows, next_ids, 2)?,
        ],
    ))
}

pub(super) async fn prove(runtime: &Runtime, identity: &str) -> Result<Value> {
    let before_domain = fixtures::domain(runtime).await?;
    let ids = stream::iter((942_700..942_707).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut ids, school| async move {
            ids.push(cli::seed(runtime, "CO", school).await?);
            Ok(ids)
        })
        .await?;
    let before = checkpoints(runtime, &ids).await?;
    let path = cli::config(runtime, "CO", 5, 3, "pipeline-focused-config.toml")?;
    let generation = format!("pipeline-{identity}");
    let first = cli::call(
        runtime,
        &[
            "crawl",
            "--config",
            path.to_str().context("config path notUTF8")?,
            "--generation",
            &generation,
            "--hours",
            "1",
            "--max-batches",
            "2",
        ],
    )
    .await?;
    let request = runtime.args.evidence.join("pipeline-request.json");
    io::save(
        &request,
        first.get("request").context("pipeline request absent")?,
    )?;
    let duplicate = cli::call(
        runtime,
        &[
            "submit",
            "--request",
            request.to_str().context("request path notUTF8")?,
        ],
    )
    .await?;
    let id = io::text(&first, "/submission/invocationId")?;
    ensure!(
        id == io::invocation(&duplicate)?,
        "pipeline submissions did not deduplicate"
    );
    let report = runtime
        .http
        .output(&runtime.ingress, id, "progress")
        .await?;
    cli::progress(&report, 5, 5)?;
    let next_key = format!("{generation}-000001");
    ensure!(
        io::text(&report, "/continuation")? == next_key,
        "pipeline first batch did not continue"
    );
    let next = cli::continuation(runtime, &next_key).await?;
    cli::progress(&next, 2, 2)?;
    ensure!(
        next.get("continuation") == Some(&Value::Null)
            && io::text(&next, "/stopped_reason")? == cli::BOUNDED,
        "pipeline continuation exceeded bounded run"
    );
    let (children, journals) = children(runtime, &first, &next_key, &generation, &ids).await?;
    let operations:Value=sqlx::query_scalar("SELECT jsonb_agg(jsonb_build_object('page_id',page_id,'state',state,'original_attempts',original_attempts) ORDER BY page_id) FROM durable_page_operations WHERE run_generation=$1").bind(format!("{generation}-schools")).fetch_one(&runtime.pool).await?;
    ensure!(
        operations
            == json!(
                ids.iter()
                    .map(|page| json!({"page_id":page,"state":"rejected","original_attempts":3}))
                    .collect::<Vec<_>>()
            ),
        "pipeline lost seven settled rejected operations"
    );
    ensure!(
        checkpoints(runtime, &ids).await? == before,
        "pipeline changed original checkpoint fields"
    );
    check_domain(runtime, &before_domain).await?;
    let proof = json!({"invocation_id":id,"first":first,"duplicate":duplicate,"report":report,"continuation":next,"continuation_key":next_key,"page_ids":ids,"original_checkpoints":before,"operations":operations,"children":children,"journals":journals,"public_source_requests":0,"checkpoint_counters_and_errors_unchanged":true});
    io::save(&runtime.args.evidence.join("pipeline-proof.json"), &proof)?;
    Ok(proof)
}

pub(super) async fn restored(runtime: &Runtime, proof: &Value) -> Result<Value> {
    let root = cli::call(
        runtime,
        &["status", "--key", io::text(proof, "/first/workflow_key")?],
    )
    .await?;
    let next = cli::call(
        runtime,
        &["status", "--key", io::text(proof, "/continuation_key")?],
    )
    .await?;
    ensure!(
        Some(&root) == proof.get("report") && Some(&next) == proof.get("continuation"),
        "pipeline reports changed after backup restore"
    );
    let ids = io::array(proof, "/page_ids")?
        .iter()
        .map(|id| id.as_i64().context("checkpoint ID invalid"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        Some(&checkpoints(runtime, &ids).await?) == proof.get("original_checkpoints"),
        "pipeline checkpoint fields changed after backup restore"
    );
    let wires: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_wire_requests")
        .fetch_one(&runtime.pool)
        .await?;
    ensure!(wires == 0, "restored pipeline attempted public HTTP");
    Ok(
        json!({"root_report_unchanged":true,"continuation_report_unchanged":true,"seven_checkpoint_records_unchanged":true,"public_source_requests":0}),
    )
}
