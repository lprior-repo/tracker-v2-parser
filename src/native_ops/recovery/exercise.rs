use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::{Value, json};

use super::{cli, fixtures, io, journal, pipeline, review, runtime::Runtime, workbook};

pub(super) async fn run(runtime: &mut Runtime, identity: &str) -> Result<Value> {
    let capacity = super::capacity::prove(runtime, identity).await?;
    let mut workflow = cli::prove(runtime, identity).await?;
    let reviews = review::seed(runtime).await?;
    io::save(
        &runtime.args.evidence.join("review-checkpoints.json"),
        &json!(reviews),
    )?;
    let mut requests = Vec::new();
    let mut replays = Vec::new();
    for source in ["athleticnet", "milesplit"] {
        let request = fixtures::request(&runtime.root, identity, source)?;
        io::save(
            &runtime.args.evidence.join(format!(
                "{}-request.json",
                io::text(&request, "/operation_id")?
            )),
            &request,
        )?;
        let (id, result) = fixtures::replay(runtime, &request).await?;
        requests.push(request.clone());
        replays.push((request, id, result));
    }
    let counts = fixtures::domain(runtime).await?;
    fixtures::verify_domain(&counts)?;
    fixtures::retained(&counts, &requests, &runtime.root)?;
    let mut pipeline = pipeline::prove(runtime, identity).await?;
    let counts = fixtures::domain(runtime).await?;
    let mut config = fixtures::configuration();
    config
        .get_mut("export")
        .and_then(Value::as_object_mut)
        .context("export config absent")?
        .insert("review_sheet".into(), json!(true));
    let request = json!({"operation_id":format!("export-{identity}"),"spec":{"kind":"export","config":config}});
    io::save(&runtime.args.evidence.join("export-request.json"), &request)?;
    let id = runtime.restart_pending(&request).await?;
    let result = runtime
        .http
        .output(&runtime.ingress, &id, "receipt")
        .await?;
    let settings = runtime.settings().await?;
    let runtime = &*runtime;
    let workflow_restore = cli::restored(runtime, &workflow).await?;
    let pipeline_restore = pipeline::restored(runtime, &pipeline).await?;
    workflow
        .as_object_mut()
        .context("workflow proof must be object")?
        .insert("after_restore".into(), workflow_restore.clone());
    pipeline
        .as_object_mut()
        .context("pipeline proof must be object")?
        .insert("after_restore".into(), pipeline_restore.clone());
    let verified = workbook::verify(runtime, &result, &reviews)?;
    replays.push((request, id.clone(), result.clone()));
    stream::iter(replays.iter().map(Ok::<_, anyhow::Error>))
        .try_for_each(|(request, id, expected)| async move {
            let (actual, _) = runtime.http.submit(&runtime.ingress, request).await?;
            ensure!(
                &actual == id
                    && runtime
                        .http
                        .output(&runtime.ingress, &actual, "receipt")
                        .await?
                        == *expected,
                "invocation or output changed after restore/replay"
            );
            Ok(())
        })
        .await?;
    fixtures::retained(&counts, &requests, &runtime.root)?;
    ensure!(
        fixtures::domain(runtime).await? == counts,
        "domain facts changed after replay/restore"
    );
    ensure!(
        workbook::verify(runtime, &result, &reviews)? == verified,
        "immutable workbook or pointer changed on duplicate export"
    );
    io::save(&runtime.args.evidence.join("export-result.json"), &result)?;
    io::save(
        &runtime.args.evidence.join("export-verification.json"),
        &verified,
    )?;
    let retained = journal::export(runtime, &id).await?;
    let isolation = workbook::isolate(runtime, &verified, &reviews)?;
    io::save(
        &runtime.args.evidence.join("delivery-isolation.json"),
        &isolation,
    )?;
    Ok(
        json!({"capacity_retry":capacity,"offline_invocation_ids":replays.iter().take(2).map(|(_,id,_)|id).collect::<Vec<_>>(),"export_invocation_id":id,"domain":counts,"export":verified,"workflow_cli":workflow,"workflow_restore":workflow_restore,"pipeline":pipeline,"pipeline_restore":pipeline_restore,"journal":retained,"delivery_copy_isolation":isolation,"service_settings_after_restore":settings,"backup_restore":serde_json::from_slice::<Value>(&io::read(&runtime.args.evidence.join("restart-proof.json"),io::JSON_LIMIT)?)?}),
    )
}
