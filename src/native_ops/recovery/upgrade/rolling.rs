mod scenario;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::{Value, json};

use super::{
    super::{cli, io, runtime::Runtime},
    evidence, handoff,
};

pub(super) async fn prove(runtime: &mut Runtime, identity: &str) -> Result<Value> {
    let view = &*runtime;
    let pages = stream::iter((942_900..942_905).map(Ok::<_, anyhow::Error>))
        .try_fold(Vec::new(), |mut pages, school| async move {
            pages.push(cli::seed(view, "IL", school).await?);
            Ok(pages)
        })
        .await?;
    let before = handoff::checkpoints(runtime, &pages).await?;
    let fixture = crate::durable::scheduler_proof::Worker::start(
        &runtime.database,
        *pages.first().context("slow fixture page absent")?,
    )
    .await?;
    let result = run_fixture(runtime, identity, &pages, fixture.endpoint()).await;
    let stopped = fixture.stop().await;
    let restored = runtime
        .http
        .request(
            &runtime.admin,
            "/deployments",
            Some(&json!({"uri":runtime.endpoint,"force":true})),
            None,
        )
        .await;
    stopped?;
    let (status, registration) = restored?;
    ensure!(
        matches!(status, 200 | 201),
        "original Page worker restoration failed HTTP {status}"
    );
    io::save(
        &runtime
            .args
            .evidence
            .join("scheduler-original-worker-restored.json"),
        &registration,
    )?;
    let (status, route) = runtime
        .http
        .request(&runtime.admin, "/services/AthleticPage", None, None)
        .await?;
    ensure!(
        status == 200 && route.get("deployment_id") == registration.get("id"),
        "original Page worker routing was not restored"
    );
    let mut proof = result?;
    ensure!(
        handoff::checkpoints(runtime, &pages).await? == before,
        "rolling replay changed original failed checkpoint fields"
    );
    let object = proof
        .as_object_mut()
        .context("rolling proof must be object")?;
    object.insert("checkpoint_records_unchanged".into(), json!(true));
    object.insert("fixture_tasks_drained".into(), json!(true));
    object.insert("fixture_scope".into(), json!("production root scheduler and real configured reservation; AthleticPage local fixture adapter delays one exhausted page without provider HTTP"));
    io::save(
        &runtime.args.evidence.join("rolling-restart-proof.json"),
        &proof,
    )?;
    Ok(proof)
}

async fn run_fixture(
    runtime: &mut Runtime,
    identity: &str,
    pages: &[i64],
    endpoint: &str,
) -> Result<Value> {
    let registered = runtime
        .register_fixture(endpoint, "scheduler-fixture-registration.json")
        .await?;
    let (status, route) = runtime
        .http
        .request(&runtime.admin, "/services/AthleticPage", None, None)
        .await?;
    ensure!(
        status == 200 && route.get("deployment_id") == registered.get("id"),
        "scheduler fixture Page routing mismatch"
    );
    let request = scenario::request(runtime, identity)?;
    let path = runtime.args.evidence.join("rolling-request.json");
    io::save(&path, &request)?;
    let submission = cli::call(
        runtime,
        &[
            "submit",
            "--request",
            path.to_str().context("rolling request path not UTF8")?,
        ],
    )
    .await?;
    let id = io::invocation(&submission)?.to_owned();
    evidence::identifier(&id, "inv_")?;
    let generation = io::text(&request, "/generation")?;
    let children = scenario::wait_refill(runtime, generation).await?;
    let prefix = evidence::journal(runtime, &id).await?;
    let attempts = evidence::no_attempts(runtime).await?;
    io::save(
        &runtime
            .args
            .evidence
            .join("rolling-active-before-restart.json"),
        &json!({"root":id,"children":children,"journal":prefix,"attempts":attempts}),
    )?;
    let restart = runtime.restart_active(&id).await?;
    let report = runtime
        .http
        .output(&runtime.ingress, &id, "progress")
        .await?;
    cli::progress(&report, 5, 5)?;
    ensure!(
        report.get("continuation") == Some(&Value::Null)
            && io::text(&report, "/stopped_reason")? == cli::BOUNDED,
        "rolling proof exceeded one batch"
    );
    let duplicate = cli::call(
        runtime,
        &[
            "submit",
            "--request",
            path.to_str().context("rolling request path not UTF8")?,
        ],
    )
    .await?;
    ensure!(
        io::invocation(&duplicate)? == id,
        "rolling replay changed invocation identity"
    );
    ensure!(
        runtime
            .http
            .output(&runtime.ingress, &id, "progress")
            .await?
            == report,
        "rolling duplicate submission changed retained output"
    );
    scenario::finish(
        runtime,
        scenario::Completed {
            id: &id,
            generation,
            pages,
            children: &children,
            prefix: &prefix,
            report: &report,
            restart: &restart,
        },
    )
    .await
}
