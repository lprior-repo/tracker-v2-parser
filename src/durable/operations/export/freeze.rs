use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::Value;

use super::{Batch, FrozenChunk, FrozenExport, FrozenPlan, OperationSpec, Workspace};
use crate::{
    config::Config,
    database::{Database, ExportSnapshot},
    export::ExportLayout,
    model::{AthleteProfileDetails, AthleteRecord, Source},
};

fn generation(workspace: &Workspace) -> Result<u32> {
    let generation = (1..=100_u32)
        .find(|number| {
            workspace
                .path(&format!("freeze-generation-{number}.json"))
                .is_ok_and(|path| !path.exists())
        })
        .context("export freeze exceeds 100 recovery generations")?;
    workspace.record(
        &format!("freeze-generation-{generation}.json"),
        &serde_json::json!({"binding":workspace.binding(),"generation":generation}),
    )?;
    Ok(generation)
}

fn profile_for_sheet(mut profile: AthleteProfileDetails) -> AthleteProfileDetails {
    let pages = if profile.source == Source::MileSplit {
        profile
            .source_values
            .get("pages")
            .and_then(Value::as_object)
    } else {
        None
    };
    let pages = pages
        .into_iter()
        .flatten()
        .filter_map(|(url, page)| {
            let rows = page.get("pr_rows")?.as_array()?;
            let rows = rows
                .iter()
                .filter_map(|row| {
                    let html = row.get("html")?.as_str()?;
                    Some(serde_json::json!({"html": html, "masked": row.get("masked")}))
                })
                .collect::<Vec<_>>();
            if rows.is_empty() {
                None
            } else {
                Some((url.clone(), serde_json::json!({"pr_rows": rows})))
            }
        })
        .collect::<serde_json::Map<String, Value>>();
    profile.source_values = serde_json::json!({"pages": pages});
    profile
}

fn performance_values(values: Option<Value>) -> Option<Value> {
    values.and_then(|values| {
        let fields = ["level", "revision_status"]
            .into_iter()
            .filter_map(|field| {
                values
                    .get(field)
                    .map(|value| (field.to_owned(), value.clone()))
            })
            .collect::<serde_json::Map<String, Value>>();
        (!fields.is_empty()).then_some(Value::Object(fields))
    })
}

fn record_for_sheet(mut record: AthleteRecord) -> AthleteRecord {
    record.profile_details = record
        .profile_details
        .into_iter()
        .map(profile_for_sheet)
        .collect();
    record.performances = record
        .performances
        .into_iter()
        .map(|mut evidence| {
            evidence.performance.source_values =
                performance_values(evidence.performance.source_values);
            evidence
        })
        .collect();
    record
}

fn append(workspace: &Workspace, mut plan: FrozenPlan, batch: &Batch) -> Result<FrozenPlan> {
    ensure!(
        plan.chunks.len() < 10000,
        "frozen export exceeds 10000 chunks"
    );
    let ids = batch
        .records
        .iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    ensure!(
        ids.iter().all(|id| *id > 0) && ids.windows(2).all(|pair| pair.first() < pair.get(1)),
        "frozen athlete IDs must be strictly ascending"
    );
    let bytes = serde_json::to_vec(batch)?;
    let total = plan
        .bytes
        .checked_add(u64::try_from(bytes.len())?)
        .context("frozen export size overflow")?;
    ensure!(total <= 512 * 1024 * 1024, "frozen export exceeds 512 MiB");
    let artifact = workspace.store(
        &format!(
            "freeze-{}-chunk-{}.json",
            plan.generation,
            plan.chunks.len()
        ),
        &bytes,
    )?;
    plan.rows = plan
        .rows
        .checked_add(ids.len())
        .context("frozen export row overflow")?;
    plan.bytes = total;
    plan.chunks.push(FrozenChunk {
        artifact,
        athlete_ids: ids,
    });
    Ok(plan)
}

async fn collect(
    snapshot: ExportSnapshot<'_>,
    workspace: &Workspace,
    plan: FrozenPlan,
) -> Result<FrozenPlan> {
    let batches = stream::try_unfold(snapshot, |mut snapshot| async move {
        if let Some(records) = snapshot.next_records(1000).await? {
            return Ok(Some((
                Batch {
                    records: records.into_iter().map(record_for_sheet).collect(),
                    issues: Vec::new(),
                },
                snapshot,
            )));
        }
        if let Some(issues) = snapshot.next_issues().await? {
            let issues = issues
                .into_iter()
                .map(|issue| (issue.source, issue.url, issue.detail))
                .collect();
            return Ok(Some((
                Batch {
                    records: Vec::new(),
                    issues,
                },
                snapshot,
            )));
        }
        snapshot.finish().await?;
        Ok::<_, anyhow::Error>(None)
    });
    batches.try_fold(plan, |plan, batch| {
        let workspace = workspace.clone();
        async move { tokio::task::spawn_blocking(move || append(&workspace, plan, &batch)).await? }
    }).await
}

#[tracing::instrument(skip_all)]
pub async fn freeze_export(
    workspace: &Workspace,
    config: &Config,
    database_url: &str,
) -> Result<FrozenExport> {
    let mut config = config.clone();
    config.validate()?;
    workspace.validate_spec(&OperationSpec::Export {
        config: config.clone(),
    })?;
    ensure!(
        config.export.layout == ExportLayout::SingleSheet,
        "durable export requires one Athletes sheet"
    );
    if let Some(plan) = workspace.load::<FrozenPlan>("frozen-export.json")? {
        ensure!(
            plan.binding == workspace.binding(),
            "frozen export binding mismatch"
        );
        plan.chunks
            .iter()
            .try_for_each(|chunk| workspace.read(&chunk.artifact).map(|_| ()))?;
        return Ok(FrozenExport {
            manifest: workspace.record("frozen-export.json", &plan)?,
        });
    }
    let generation = generation(workspace)?;
    let database = Database::connect_for_export(database_url).await?;
    let result = async {
        let snapshot = database
            .begin_export_snapshot(&config.states, &config.graduation_years)
            .await?;
        let plan = FrozenPlan {
            binding: workspace.binding().into(),
            generation,
            config,
            chunks: Vec::new(),
            rows: 0,
            bytes: 0,
        };
        collect(snapshot, workspace, plan).await
    }
    .await;
    database.close().await;
    let plan = result?;
    Ok(FrozenExport {
        manifest: workspace.record("frozen-export.json", &plan)?,
    })
}
