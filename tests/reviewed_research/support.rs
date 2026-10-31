use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{Artifact, OperationSpec, Workspace, execute_import, stage_input},
    model::Source,
};

use crate::support::Scenario;

pub fn address(source_id: &str, school_id: i64) -> Value {
    json!({
        "kind":"school_address",
        "source":"athleticnet",
        "source_id":source_id,
        "expected_school_id":school_id,
        "expected_name":"Test High School",
        "expected_city":"Madison",
        "expected_state":"WI",
        "address":"2 Oak Street",
        "postal_code":"53703",
        "evidence":[0]
    })
}

pub fn coach(school_id: i64) -> Value {
    json!({
        "kind":"coach",
        "source":"athleticnet",
        "source_id":"school-1",
        "expected_school_id":school_id,
        "expected_name":"Test High School",
        "expected_city":"Madison",
        "expected_state":"WI",
        "name":"Jordan Coach",
        "sport":"outdoor",
        "gender":"girls",
        "role":"Head Girls Track Coach",
        "email":"jordan@example.edu",
        "name_role_evidence":[0],
        "email_evidence":[1]
    })
}

pub fn envelope(changes: Vec<Value>) -> Value {
    json!({
        "schema_revision":1,
        "reviewed_by":"codex",
        "researcher":"gpt-6-luna",
        "authorization_quote":"I authorize applying these reviewed findings.",
        "reviewed_at":"2026-10-30T14:00:00Z",
        "evidence": [
            {"artifact":null,"primary_url":"https://school.example.edu/staff","captured_at":"2026-10-30T13:00:00Z","capture_kind":"web_tool_text"},
            {"artifact":null,"primary_url":"https://school.example.edu/directory","captured_at":"2026-10-30T13:01:00Z","capture_kind":"browser_dom"}
        ],
        "changes":changes
    })
}

pub fn build(
    test: &Scenario,
    operation_id: &str,
    mut body: Value,
) -> Result<(Workspace, OperationSpec, Vec<Artifact>)> {
    let root = test.directory().join("research-artifacts");
    let first = stage_capture(&root, b"public staff capture")?;
    let second = stage_capture(&root, b"email directory capture")?;
    body["evidence"][0]["artifact"] = serde_json::to_value(&first)?;
    body["evidence"][1]["artifact"] = serde_json::to_value(&second)?;
    let source = test.directory().join(format!("{operation_id}.json"));
    let encoded = serde_json::to_vec(&body)?;
    std::fs::write(&source, &encoded)?;
    let manifest = stage_input(&root, &source)?;
    let spec = OperationSpec::ReviewedResearch { manifest };
    let workspace = Workspace::open(&root, operation_id, &spec)?;
    Ok((workspace, spec, vec![first, second]))
}

fn stage_capture(root: &std::path::Path, bytes: &[u8]) -> Result<Artifact> {
    let source = tempfile::NamedTempFile::new()?;
    std::fs::write(source.path(), bytes)?;
    Ok(stage_input(root, source.path())?)
}

pub async fn apply(test: &Scenario, workspace: &Workspace, spec: &OperationSpec) -> Result<Value> {
    Ok(execute_import(workspace, spec, &test.url).await?)
}

pub async fn insert_school(test: &Scenario, source_id: &str) -> Result<i64> {
    let database = Database::connect(&test.url).await?;
    let school_id = sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,city,source_url) VALUES($1,$2,'Test High School','WI','Madison','https://school.example.edu') RETURNING id")
        .bind(Source::AthleticNet.as_str())
        .bind(source_id)
        .fetch_one(&test.pool)
        .await?;
    database.close().await;
    Ok(school_id)
}
