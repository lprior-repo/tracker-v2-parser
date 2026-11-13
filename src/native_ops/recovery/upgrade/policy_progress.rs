mod scope;
mod seed;
mod verify;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::{Value, json};

use super::super::{cli, io, runtime::Runtime};
use crate::durable::model::SchedulerVersion;

#[tracing::instrument(skip_all)]
pub(super) async fn prove(runtime: &Runtime, identity: &str) -> Result<Value> {
    scope::prove(runtime, identity).await
}

async fn scenarios(runtime: &mut Runtime, identity: &str) -> Result<Value> {
    let scenarios = [
        ("ND", 1, None),
        ("VT", 3, None),
        ("UT", 3, Some(SchedulerVersion::RollingV2)),
    ];
    let (runtime, proofs) = stream::iter(scenarios.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (runtime, Vec::new()),
            |(runtime, mut proofs), (state, concurrency, scheduler)| async move {
                let seeded =
                    seed::prepare(runtime, identity, state, scheduler, concurrency, 100).await?;
                proofs.push(exercise(runtime, &seeded).await?);
                Ok((runtime, proofs))
            },
        )
        .await?;
    let proof = json!({"status":"passed","schedulers":proofs,"same100_candidate_scan_bound":true,"unchanged3_turn_and2_batch_fixture_budgets":true});
    io::save(
        &runtime.args.evidence.join("policy-progress-proof.json"),
        &proof,
    )?;
    Ok(proof)
}

#[tracing::instrument(skip_all)]
async fn exercise(runtime: &mut Runtime, seeded: &seed::Seeded) -> Result<Value> {
    let path = runtime
        .args
        .evidence
        .join(format!("{}-request.json", seeded.request.generation));
    io::save(&path, &serde_json::to_value(&seeded.request)?)?;
    let path = path.to_str().context("policy request path not UTF8")?;
    let (submission, restart) = submit_for_replay(runtime, seeded, path).await?;
    let id = io::invocation(&submission)?;
    let first = runtime
        .http
        .output(&runtime.ingress, id, "progress")
        .await?;
    let next = io::text(&first, "/continuation")?;
    let second = cli::continuation(runtime, next).await?;
    io::save(
        &runtime
            .args
            .evidence
            .join(format!("{}-reports.json", seeded.request.generation)),
        &json!({"root":id,"first":first,"second":second,"request":seeded.request}),
    )?;
    let before = super::evidence::journal(runtime, id).await?;
    let duplicate = cli::call(runtime, &["submit", "--request", path]).await?;
    ensure!(
        io::invocation(&duplicate)? == id
            && runtime
                .http
                .output(&runtime.ingress, id, "progress")
                .await?
                == first,
        "policy replay replaced the request or its result"
    );
    ensure!(
        super::evidence::journal(runtime, id).await? == before,
        "policy replay changed the retained journal"
    );
    let (bytes, input) = super::evidence::input(&before)?;
    ensure!(
        input == serde_json::to_value(&seeded.request)?,
        "policy input changed"
    );
    let mut proof = verify::run(runtime, seeded, id, &first, &second).await?;
    proof
        .as_object_mut()
        .context("policy proof must be object")?
        .insert("retained_input_sha256".into(), json!(io::digest(&bytes)));
    proof
        .as_object_mut()
        .context("policy proof must be object")?
        .insert("progress_checkpoint_restart".into(), restart);
    Ok(proof)
}

#[tracing::instrument(skip_all)]
async fn submit_for_replay(
    runtime: &mut Runtime,
    seeded: &seed::Seeded,
    path: &str,
) -> Result<(Value, Value)> {
    let barrier = if seeded.request.scheduler_version.is_some() {
        let mut transaction = runtime.pool.begin().await?;
        sqlx::query("LOCK TABLE reviewed_result_links IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *transaction)
            .await?;
        Some(transaction)
    } else {
        None
    };
    let restart_needed = barrier.is_some();
    let submitted = async {
        let submission = cli::call(runtime, &["submit", "--request", path]).await?;
        let id = io::invocation(&submission)?;
        super::evidence::identifier(id, "inv_")?;
        let restart = if restart_needed {
            checkpoint_restart(runtime, id).await?
        } else {
            Value::Null
        };
        Ok::<_, anyhow::Error>((submission, restart))
    }
    .await;
    if let Some(barrier) = barrier {
        barrier.rollback().await?;
    }
    submitted
}

#[tracing::instrument(skip_all)]
async fn checkpoint_restart(runtime: &mut Runtime, id: &str) -> Result<Value> {
    let statement =
        format!("SELECT index,entry_json FROM sys_journal WHERE id='{id}' ORDER BY index");
    let before = super::evidence::wait(runtime, &statement, |rows| {
        let Ok(entries) = rows
            .iter()
            .map(super::evidence::decode)
            .collect::<Result<Vec<_>>>()
        else {
            return false;
        };
        entries
            .iter()
            .filter_map(|entry| entry.pointer("/Notification/Completion/Run"))
            .any(|run| {
                super::evidence::contains_json_value(run, &json!({"selection":"policy_advanced"}))
            })
    })
    .await?;
    io::save(
        &runtime
            .args
            .evidence
            .join("policy-progress-before-restart.json"),
        &json!(before),
    )?;
    let restart = runtime.restart_active(id).await?;
    let after = super::evidence::journal(runtime, id).await?;
    ensure!(
        after.get(..before.len()) == Some(before.as_slice()),
        "policy checkpoint prefix changed after native restore"
    );
    Ok(
        json!({"restart":restart,"prefix_entries":before.len(),"prefix_unchanged":true,"same_invocation":id}),
    )
}
