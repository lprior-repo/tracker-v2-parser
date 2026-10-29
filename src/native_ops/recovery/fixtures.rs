use std::{fs, path::Path};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{io, runtime::Runtime};

pub(super) fn configuration() -> Value {
    json!({"acquisition":{"mode":"complete"},"states":["OH","WI"],"graduation_years":[2027,2028],"season":2026,"limits":{"requests_per_second":2.0,"max_pages":100,"max_attempts":3,"concurrency":1},"sources":{"athleticnet":{"enabled":true,"transport":"http"},"milesplit":{"enabled":true,"transport":"http"}},"export":{"rows_per_file":2000,"per_athlete_sheets":false,"layout":"single_sheet"}})
}

pub(super) fn request(root: &Path, identity: &str, source: &str) -> Result<Value> {
    let (fixture, athlete, state, url) = match source {
        "athleticnet" => (
            "athleticnet/bio-28872883.json",
            "28872883",
            "WI",
            "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0",
        ),
        "milesplit" => (
            "milesplit/athlete-oh-15005189.html",
            "15005189",
            "OH",
            "https://oh.milesplit.com/athletes/15005189-brice-fuller",
        ),
        _ => anyhow::bail!("unsupported owned fixture source"),
    };
    let bytes = io::read(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture),
        16 * 1024 * 1024,
    )?;
    let inputs = root.join("inputs");
    if !inputs.try_exists()? {
        fs::create_dir(&inputs)?;
    }
    let hash = io::digest(&bytes);
    let path = inputs.join(format!("{hash}.input"));
    io::write(&path, &bytes)?;
    Ok(
        json!({"operation_id":format!("offline-{source}-{identity}"),"spec":{"kind":"offline_replay","config":configuration(),"target":{"source":source,"kind":"athlete","url":url,"context":{"state":state,"athlete_id":athlete,"sport":"outdoor","season":2026}},"capture":{"path":path,"sha256":hash,"bytes":bytes.len()}}}),
    )
}

pub(super) async fn replay(runtime: &Runtime, request: &Value) -> Result<(String, Value)> {
    let (id, first) = runtime.http.submit(&runtime.ingress, request).await?;
    let (duplicate, second) = runtime.http.submit(&runtime.ingress, request).await?;
    ensure!(
        id == duplicate,
        "duplicate fixture submissions changed invocation"
    );
    let result = runtime
        .http
        .output(&runtime.ingress, &id, "receipt")
        .await?;
    io::artifact(
        result
            .get("receipt")
            .ok_or_else(|| anyhow::anyhow!("missing fixture receipt"))?,
        &runtime.root,
    )?;
    io::save(
        &runtime.args.evidence.join(format!(
            "{}-replay.json",
            io::text(request, "/operation_id")?
        )),
        &json!({"invocation_id":id,"first":first,"duplicate":second,"result":result,"domain":domain(runtime).await?}),
    )?;
    Ok((id, result))
}

pub(super) async fn domain(runtime: &Runtime) -> Result<Value> {
    Ok(sqlx::query_scalar(include_str!("domain.sql"))
        .fetch_one(&runtime.pool)
        .await?)
}

pub(super) fn verify_domain(value: &Value) -> Result<()> {
    let expected = [
        ("athletes", 2),
        ("profiles", 2),
        ("results", 81),
        ("individual", 69),
        ("relay", 12),
        ("page_receipts", 2),
        ("completed_operations", 2),
        ("source_wire_requests", 0),
        ("succeeded_pages", 2),
        ("fixture_profile", 1),
        ("specific_mark", 1),
        ("attempts", 5),
        ("fixture_replay_attempts", 2),
        ("seeded_legacy_attempts", 3),
        ("athleticnet_results", 53),
        ("milesplit_results", 28),
        ("milesplit_prs", 28),
        ("unknown_year_profile", 1),
        ("published_year_profile", 1),
        ("milesplit_specific_prs", 6),
    ];
    expected.into_iter().try_for_each(|(key, count)| {
        ensure!(
            value.get(key).and_then(Value::as_i64) == Some(count),
            "fixture domain mismatch for {key}"
        );
        Ok(())
    })
}

pub(super) fn retained(value: &Value, requests: &[Value], root: &Path) -> Result<()> {
    let receipts = io::array(value, "/retained_receipts")?;
    ensure!(
        receipts.len() == 2 && requests.len() == 2,
        "proof requires both retained fixture receipts"
    );
    requests.iter().try_for_each(|request| -> Result<()> {
        io::artifact(
            request
                .pointer("/spec/capture")
                .ok_or_else(|| anyhow::anyhow!("missing fixture input"))?,
            root,
        )?;
        let url = io::text(request, "/spec/target/url")?;
        ensure!(
            receipts.iter().any(
                |receipt| receipt.pointer("/capture/url").and_then(Value::as_str) == Some(url)
            ),
            "retained receipts lost exact source URL"
        );
        Ok(())
    })?;
    receipts.iter().try_for_each(|receipt|->Result<()> {
        ["capture","parsed"].into_iter().try_for_each(|stage|->Result<()> {
            let reference=receipt.pointer(&format!("/{stage}/body")).ok_or_else(||anyhow::anyhow!("missing retained artifact"))?;
            let hash=io::text(reference,"/sha256")?;
            ensure!(hash.len()==64 && hash.bytes().all(|b|b.is_ascii_hexdigit()),"invalid retained SHA256");
            let descriptor=json!({"path":root.join(format!("{hash}.blob")),"sha256":hash,"bytes":reference.get("bytes")});
            io::artifact(&descriptor,root)?;Ok(())
        })
    })
}
