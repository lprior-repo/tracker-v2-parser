use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::super::{
    super::{cli, io, journal, runtime::Runtime},
    evidence,
};
use crate::durable::model::{CrawlPhase, CrawlRequest, SchedulerVersion};

pub(super) fn request(runtime: &Runtime, identity: &str) -> Result<Value> {
    let path = cli::config(runtime, "IL", 5, 3, "rolling-config.toml")?;
    Ok(serde_json::to_value(CrawlRequest {
        generation: format!("rolling-{identity}"),
        config: crate::config::Config::load(&path)?,
        deadline_ms: chrono::Utc::now()
            .timestamp_millis()
            .checked_add(3_600_000)
            .context("rolling deadline overflow")?,
        cooldown_ms: 15_000,
        batch: 0,
        max_batches: 1,
        phase: CrawlPhase::SchoolChains,
        inherited_retry_campaigns: vec![format!("ancestor-{identity}")],
        scheduler_version: Some(SchedulerVersion::RollingV2),
        deadline_extension: None,
    })?)
}

fn query(generation: &str) -> Result<String> {
    ensure!(
        generation.len() < 128
            && generation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "owned rolling generation invalid"
    );
    Ok(format!(
        "SELECT id,idempotency_key,status,created_at,completed_at FROM sys_invocation WHERE target_service_name='AthleticPage' AND idempotency_key LIKE '{generation}-%' ORDER BY idempotency_key"
    ))
}

pub(super) async fn wait_refill(runtime: &Runtime, generation: &str) -> Result<Vec<Value>> {
    let statement = query(generation)?;
    let deadline = io::deadline(std::time::Duration::from_mins(1))?;
    let (found, samples) = futures::stream::iter((0..300).map(Ok::<_, anyhow::Error>))
        .try_fold((None, Vec::new()), |(found, mut samples), tick| {
            let statement = &statement;
            async move {
                if found.is_some() || tokio::time::Instant::now() >= deadline {
                    return Ok((found, samples));
                }
                let rows = evidence::query(runtime, statement).await?;
                if tick % 5 == 0 {
                    samples.push(json!({"utc":chrono::Utc::now().to_rfc3339(),"children":rows}));
                }
                if refilled(&rows) {
                    return Ok((Some(rows), samples));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                Ok((None, samples))
            }
        })
        .await?;
    io::save(
        &runtime
            .args
            .evidence
            .join("rolling-refill-observations.json"),
        &json!(samples),
    )?;
    let Some(rows) = found else {
        save_failure(runtime, generation).await?;
        anyhow::bail!("owned slow-first refill condition was not observed within 60 seconds");
    };
    pending_timer(runtime, rows.first().context("slow child absent")?).await?;
    ensure!(
        rows.iter()
            .filter(|row| row.get("status").and_then(Value::as_str) != Some("completed"))
            .count()
            == 1,
        "slow-first fixture must retain exactly one pending child"
    );
    Ok(rows)
}

fn refilled(rows: &[Value]) -> bool {
    rows.len() == 5
        && rows.first().is_some_and(|row| {
            matches!(
                row.get("status").and_then(Value::as_str),
                Some("running" | "suspended")
            ) && row.get("completed_at").is_none_or(Value::is_null)
        })
        && rows
            .iter()
            .skip(1)
            .all(|row| row.get("status").and_then(Value::as_str) == Some("completed"))
}

async fn pending_timer(runtime: &Runtime, child: &Value) -> Result<()> {
    let id = io::text(child, "/id")?;
    let rows = evidence::journal(runtime, id).await?;
    let entries = rows
        .iter()
        .map(evidence::decode)
        .collect::<Result<Vec<_>>>()?;
    let sleeps = entries
        .iter()
        .filter_map(|entry| entry.pointer("/Command/Sleep"))
        .collect::<Vec<_>>();
    ensure!(
        sleeps.len() == 1,
        "slow fixture must retain exactly one durable timer"
    );
    let sleep = sleeps.first().context("slow timer absent")?;
    ensure!(
        sleep
            .get("wake_up_time")
            .and_then(Value::as_i64)
            .is_some_and(|wake| wake > chrono::Utc::now().timestamp_millis()),
        "slow fixture timer already elapsed before refill proof"
    );
    ensure!(
        entries.iter().all(
            |entry| entry.pointer("/Notification/Completion/Sleep").is_none()
                && entry.pointer("/Command/Output").is_none()
        ),
        "slow fixture already completed its timer or output"
    );
    io::save(
        &runtime.args.evidence.join("rolling-pending-timer.json"),
        &json!({"child":child,"durable_sleep":sleep,"journal":rows,"timer_pending":true,"output_absent":true}),
    )
}

async fn save_failure(runtime: &Runtime, generation: &str) -> Result<()> {
    let children = evidence::query(runtime, &query(generation)?).await?;
    let roots = evidence::query(runtime, &format!("SELECT id,status,target FROM sys_invocation WHERE target='AthleticCrawl/{generation}-000000/run'")).await?;
    let journals = futures::stream::iter(
        children
            .iter()
            .chain(roots.iter())
            .map(Ok::<_, anyhow::Error>),
    )
    .try_fold(Vec::new(), |mut journals, invocation| async move {
        let id = io::text(invocation, "/id")?;
        journals.push(json!({"id":id,"journal":evidence::journal(runtime,id).await?}));
        Ok(journals)
    })
    .await?;
    io::save(
        &runtime.args.evidence.join("rolling-refill-failure.json"),
        &json!({"children":children,"roots":roots,"journals":journals}),
    )
}

pub(super) struct Completed<'a> {
    pub id: &'a str,
    pub generation: &'a str,
    pub pages: &'a [i64],
    pub children: &'a [Value],
    pub prefix: &'a [Value],
    pub report: &'a Value,
    pub restart: &'a Value,
}

pub(super) async fn finish(runtime: &Runtime, case: Completed<'_>) -> Result<Value> {
    let children = evidence::query(runtime, &query(case.generation)?).await?;
    ensure!(
        children.len() == 5
            && children
                .iter()
                .all(|child| child.get("status").and_then(Value::as_str) == Some("completed")),
        "rolling children were not all drained"
    );
    let exact = |rows: &[Value]| {
        rows.iter()
            .map(|row| {
                Ok((
                    io::text(row, "/id")?.to_owned(),
                    io::text(row, "/idempotency_key")?.to_owned(),
                ))
            })
            .collect::<Result<Vec<_>>>()
    };
    ensure!(
        exact(&children)? == exact(case.children)?,
        "rolling restart added or replaced children"
    );
    let expected = (0..5)
        .map(|index| format!("{}-000000-page-{index:06}", case.generation))
        .collect::<Vec<_>>();
    ensure!(
        children
            .iter()
            .map(|row| io::text(row, "/idempotency_key"))
            .collect::<Result<Vec<_>>>()?
            .iter()
            .copied()
            .eq(expected.iter().map(String::as_str)),
        "rolling child idempotency keys changed"
    );
    let child_ids = children
        .iter()
        .map(|row| io::text(row, "/id").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let rows = journal::read(runtime, case.id, "rolling-root-journal-after-restart").await?;
    let prefix = evidence::journal(runtime, case.id).await?;
    ensure!(
        prefix.get(..case.prefix.len()) == Some(case.prefix),
        "rolling replay changed retained journal prefix"
    );
    let oracle = journal::validate_versioned_root(&rows, &child_ids, 5, true)?;
    let inputs = futures::stream::iter(children.iter().zip(case.pages).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut inputs, (child, page)| async move {
            let rows = evidence::journal(runtime, io::text(child, "/id")?).await?;
            let (_, input) = evidence::input(&rows)?;
            ensure!(
                input
                    .pointer("/request/binding/page_id")
                    .and_then(Value::as_i64)
                    == Some(*page),
                "rolling child page identity changed"
            );
            ensure!(
                input.get("scheduler_version").and_then(Value::as_str) == Some("rolling_v2"),
                "rolling child scheduler version changed"
            );
            inputs.push(input);
            Ok(inputs)
        })
        .await?;
    let attempts = evidence::no_attempts(runtime).await?;
    Ok(
        json!({"invocation_id":case.id,"report":case.report,"same_invocation_replayed":true,
        "four_fast_siblings_completed_while_first_timer_pending":true,"prefix_rows":case.prefix.len(),
        "journal_prefix_unchanged":true,"children":children,"child_inputs":inputs,
        "restart":case.restart,"oracle":oracle,"attempts":attempts,"source_http_requests":0}),
    )
}

use futures::TryStreamExt;
