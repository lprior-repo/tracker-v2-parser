#[path = "reviewed_result_links/availability.rs"]
mod availability;
#[allow(dead_code)]
mod support;
use anyhow::{Context, Result, ensure};
use calamine::{Reader, open_workbook_auto};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{Artifact, OperationSpec, Workspace, stage_input},
    export::{ExportLayout, ExportOptions, StagedExport},
    model::{
        Athlete, Capture, PageContext, PageKind, ParsedPage, School, Source, Sport, Target,
        UnassignedResult,
    },
    reviewed_result_links,
};

const PROFILE_URL: &str = "https://wi.milesplit.com/athletes/14474869-audrey-koehler";
const RESULT_URL: &str =
    "https://wi.milesplit.com/meets/790295-fall-creek-invitational/results/1336298/raw";
const RESULT_ID: &str = "raw:[\"790295\",\"milesplit-file:790295:1336298\",\"Girls 5000 Meter\",null,152,\"e95210eef37ca2ea95e60d546d22bfb62fa9804a639ed6016290005c24fcdd52\"]";
const FILE_IDENTITY: &str = "milesplit-file:790295:1336298";
const PUBLISHED_HTML: &str =
    "  19 Audrey Koehler            SR Fall Creek                                 23:03.4        ";

struct Fixture {
    database: Database,
    database_url: String,
    result_key: String,
    result_row_id: i64,
    athlete_source_id: i64,
    athlete_id: i64,
    school_id: i64,
    result: UnassignedResult,
    result_target: Target,
}

async fn fixture(test: &support::Scenario) -> Result<Fixture> {
    let database = Database::connect(&test.url).await?;
    let profile_target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        PROFILE_URL.to_owned(),
        PageContext::default(),
    );
    let profile_page = ParsedPage {
        athletes: vec![Athlete {
            id: "14474869".to_owned(),
            name: "Audrey Koehler".to_owned(),
            profile_url: Some(PROFILE_URL.to_owned()),
            gender: Some("F".to_owned()),
            grade: Some("12".to_owned()),
            graduation_year: Some(2027),
            state: Some("WI".to_owned()),
            school_id: Some("fall-creek-wi".to_owned()),
            ..Athlete::default()
        }],
        schools: vec![School {
            id: "fall-creek-wi".to_owned(),
            name: "Fall Creek".to_owned(),
            state: Some("WI".to_owned()),
            ..School::default()
        }],
        ..ParsedPage::default()
    };
    let profile_capture = Capture {
        url: PROFILE_URL.to_owned(),
        status: 200,
        content_type: "application/json".to_owned(),
        body: serde_json::to_vec(&profile_page)?,
    };
    database
        .persist(None, &profile_target, &profile_capture, &profile_page)
        .await?;
    let (athlete_source_id, athlete_id, school_id): (i64, i64, i64) = sqlx::query_as(
        "SELECT p.id,p.athlete_id,p.school_id FROM athlete_sources p WHERE p.source='milesplit' AND p.source_id='14474869'",
    )
    .fetch_one(database.pool())
    .await?;

    let witness = json!({
        "kind":"raw_result",
        "name":"Audrey Koehler",
        "team":"Fall Creek",
        "grade":"SR",
        "published_file_identity":FILE_IDENTITY,
        "line_ordinal":152,
        "published_html":PUBLISHED_HTML,
        "meet_name":"Fall Creek Invitational"
    });
    let result = UnassignedResult {
        result_id: Some(RESULT_ID.to_owned()),
        source_row: None,
        event: "Girls 5000 Meter".to_owned(),
        mark: "23:03.4".to_owned(),
        sport: Sport::CrossCountry,
        school_id: None,
        meet_id: Some("790295".to_owned()),
        date: Some("2026-10-24".to_owned()),
        is_relay: false,
        members: Vec::new(),
        raw: witness,
    };
    let result_target = Target::get(
        Source::MileSplit,
        PageKind::Results,
        RESULT_URL.to_owned(),
        PageContext::default(),
    );
    let result_page = ParsedPage {
        unassigned_results: vec![result.clone()],
        ..ParsedPage::default()
    };
    let capture = Capture {
        url: RESULT_URL.to_owned(),
        status: 200,
        content_type: "text/html".to_owned(),
        body: PUBLISHED_HTML.as_bytes().to_vec(),
    };
    database
        .persist(None, &result_target, &capture, &result_page)
        .await?;
    let result_key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            Source::MileSplit,
            "result",
            RESULT_ID
        ))?)
    );
    let result_row_id: i64 = sqlx::query_scalar(
        "SELECT id FROM results WHERE source_result_key=$1 AND athlete_source_id IS NULL AND athlete_id IS NULL",
    )
    .bind(&result_key)
    .fetch_one(database.pool())
    .await?;
    Ok(Fixture {
        database,
        database_url: test.url.clone(),
        result_key,
        result_row_id,
        athlete_source_id,
        athlete_id,
        school_id,
        result,
        result_target,
    })
}

fn staged_capture(root: &std::path::Path, name: &str, bytes: &[u8]) -> Result<Artifact> {
    std::fs::create_dir_all(root)?;
    let path = root.join(name);
    std::fs::write(&path, bytes)?;
    stage_input(root, &path)
}

fn manifest(
    fixture: &Fixture,
    test: &support::Scenario,
) -> Result<(Workspace, Artifact, OperationSpec)> {
    let root = test.directory().join("reviewed-result-artifacts");
    let result_capture = staged_capture(&root, "result.txt", PUBLISHED_HTML.as_bytes())?;
    let profile_capture = staged_capture(
        &root,
        "profile.txt",
        b"Audrey Koehler | Fall Creek | Class of 2027",
    )?;
    let value = json!({
        "schema_revision":1,
        "reviewed_by":"codex",
        "researcher":"gpt-6-luna",
        "authorization_quote":"Please manually match the verified existing result to Audrey Koehler's existing MileSplit profile.",
        "reviewed_at":"2026-10-30T14:00:00Z",
        "evidence":[
            {"artifact":result_capture,"primary_url":RESULT_URL,"captured_at":"2026-10-30T13:00:00Z","capture_kind":"retained_html"},
            {"artifact":profile_capture,"primary_url":PROFILE_URL,"captured_at":"2026-10-30T13:01:00Z","capture_kind":"web_tool_text"}
        ],
        "changes":[{
            "source":"milesplit",
            "source_result_key":fixture.result_key,
            "source_result_id":RESULT_ID,
            "source_url":RESULT_URL,
            "event":"Girls 5000 Meter",
            "mark":"23:03.4",
            "sport":"cross_country",
            "meet_id":"790295",
            "meet_date":"2026-10-24",
            "status":"recorded",
            "represented_school_id":null,
            "raw_name":"Audrey Koehler",
            "raw_team":"Fall Creek",
            "raw_grade":"SR",
            "published_file_identity":FILE_IDENTITY,
            "line_ordinal":152,
            "published_html":PUBLISHED_HTML,
            "target_source_id":"14474869",
            "target_athlete_source_id":fixture.athlete_source_id,
            "target_athlete_id":fixture.athlete_id,
            "target_profile_url":PROFILE_URL,
            "target_name":"Audrey Koehler",
            "target_gender":"F",
            "target_graduation_year":2027,
            "target_school_id":fixture.school_id,
            "target_school_name":"Fall Creek",
            "target_school_state":"WI",
            "result_evidence":[0],
            "identity_evidence":[1]
        }]
    });
    let manifest_path = test.directory().join("reviewed-result-manifest.json");
    std::fs::write(&manifest_path, serde_json::to_vec(&value)?)?;
    let artifact = stage_input(&root, &manifest_path)?;
    let spec = OperationSpec::ReviewedResultLinks {
        manifest: artifact.clone(),
    };
    let workspace = Workspace::open(&root, "reviewed-result-test", &spec)?;
    Ok((workspace, artifact, spec))
}

async fn apply(fixture: &Fixture, workspace: &Workspace, spec: &OperationSpec) -> Result<Value> {
    Ok(track_scrapper_v2::durable::operations::execute_import(
        workspace,
        spec,
        &fixture.database_url,
    )
    .await?)
}

fn manifest_with_gender(
    fixture: &Fixture,
    test: &support::Scenario,
    gender: &str,
) -> Result<(Workspace, OperationSpec)> {
    let root = test.directory().join("reviewed-result-artifacts");
    let (workspace, artifact, _) = manifest(fixture, test)?;
    let mut document: Value = serde_json::from_slice(&workspace.read(&artifact)?)?;
    document["changes"][0]["target_gender"] = json!(gender);
    let manifest_path = test
        .directory()
        .join(format!("reviewed-result-{gender}.json"));
    std::fs::write(&manifest_path, serde_json::to_vec(&document)?)?;
    let manifest_artifact = stage_input(&root, &manifest_path)?;
    let spec = OperationSpec::ReviewedResultLinks {
        manifest: manifest_artifact.clone(),
    };
    let operation_id = format!("reviewed-result-{gender}");
    let workspace = Workspace::open(&root, &operation_id, &spec)?;
    Ok((workspace, spec))
}

#[tokio::test]
async fn given_reviewed_result_when_replayed_then_link_is_idempotent_and_single_sheet_shows_observation()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let (workspace, _artifact, spec) = manifest(&fixture, test)?;
            let before: (Option<i64>, Option<i64>, bool, Value) = sqlx::query_as(
                "SELECT athlete_source_id,athlete_id,is_personal_record,raw FROM results WHERE id=$1",
            )
            .bind(fixture.result_row_id)
            .fetch_one(fixture.database.pool())
            .await?;
            let first = apply(&fixture, &workspace, &spec).await?;
            ensure!(first["links_added"] == 1 && first["links_already_present"] == 0, "first apply did not add exactly one link");
            let second = apply(&fixture, &workspace, &spec).await?;
            ensure!(second == first, "durable operation receipt did not make the repeated request idempotent");
            let receipt_count: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_operation_receipts")
                .fetch_one(fixture.database.pool())
                .await?;
            ensure!(receipt_count == 1, "repeated reviewed operation created a duplicate receipt");
            ensure!(
                sqlx::query("DELETE FROM results WHERE source_result_key=$1")
                    .bind(&fixture.result_key)
                    .execute(fixture.database.pool())
                    .await
                    .is_err(),
                "deleting a source result unexpectedly cascaded through its audit link"
            );

            let capture = Capture {
                url: fixture.result_target.url.clone(),
                status: 200,
                content_type: "text/html".to_owned(),
                body: PUBLISHED_HTML.as_bytes().to_vec(),
            };
            let replay = ParsedPage {
                unassigned_results: vec![fixture.result.clone()],
                ..ParsedPage::default()
            };
            fixture.database.persist(None, &fixture.result_target, &capture, &replay).await?;
            let after: (Option<i64>, Option<i64>, bool, Value, i64) = sqlx::query_as(
                "SELECT athlete_source_id,athlete_id,is_personal_record,raw,(SELECT count(*) FROM reviewed_result_links WHERE source_result_key=$2) FROM results WHERE id=$1",
            )
            .bind(fixture.result_row_id)
            .bind(&fixture.result_key)
            .fetch_one(fixture.database.pool())
            .await?;
            ensure!(after.0 == before.0 && after.1 == before.1 && !after.2 && after.3 == before.3 && after.4 == 1, "raw replay changed the original observation or discarded the association");

            let (records, issues) = fixture.database.export_snapshot(&["WI".to_owned()], &[2027]).await?;
            let record = records.iter().find(|record| record.id == fixture.athlete_id).context("reviewed target missing from export")?;
            ensure!(record.performances.len() == 1, "reviewed result was not exported once under the target");
            ensure!(record.performances[0].performance.mark == "23:03.4" && !record.performances[0].performance.is_personal_record, "observed result was mislabeled or lost");
            ensure!(record.performances[0].performance.result_id.as_deref() == Some(RESULT_ID), "published raw result ID was not preserved");
            ensure!(issues.iter().all(|issue| !issue.detail.contains("unassigned result")), "valid reviewed link still appears as unassigned coverage");

            let destination = test.directory().join("reviewed-observed.xlsx");
            let options = ExportOptions {
                states: vec!["WI".to_owned()],
                graduation_years: vec![2027],
                rows_per_file: 100,
                per_athlete_sheets: false,
            };
            let mut workbook_export = StagedExport::new(&destination, &options)?.with_layout(ExportLayout::SingleSheet);
            workbook_export.stage_part(&records, &[])?;
            workbook_export.finish()?;
            let mut workbook = open_workbook_auto(&destination)?;
            let sheet = workbook.worksheet_range("Athletes")?;
            let headers = sheet.rows().next().context("single-sheet headers")?;
            let header_names = headers.iter().map(ToString::to_string).collect::<Vec<_>>();
            let observed_column = headers
                .iter()
                .position(|header| header.to_string() == "XC 5000m best [unspecified; milesplit]")
                .ok_or_else(|| anyhow::anyhow!("single-sheet observed-best column missing from {header_names:?}"))?;
            let athlete_row = sheet.rows().nth(1).context("single-sheet target row")?;
            ensure!(athlete_row[observed_column].to_string().contains("23:03.4"), "single-sheet observed-best cell omitted the reviewed mark");
            let raw_after: Value = sqlx::query_scalar("SELECT raw FROM results WHERE id=$1")
                .bind(fixture.result_row_id)
                .fetch_one(fixture.database.pool())
                .await?;
            ensure!(raw_after == before.3, "export mutated raw source evidence");
            fixture.database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_girls_result_and_male_profile_then_durable_link_is_rejected() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            sqlx::query("UPDATE athletes SET gender='M' WHERE id=$1")
                .bind(fixture.athlete_id)
                .execute(fixture.database.pool())
                .await?;
            sqlx::query("UPDATE athlete_sources SET gender='M' WHERE id=$1")
                .bind(fixture.athlete_source_id)
                .execute(fixture.database.pool())
                .await?;
            let (workspace, spec) = manifest_with_gender(&fixture, test, "M")?;
            ensure!(
                apply(&fixture, &workspace, &spec).await.is_err(),
                "Girls result was linked to a known male profile and canonical athlete"
            );
            let links: i64 = sqlx::query_scalar("SELECT count(*) FROM reviewed_result_links")
                .fetch_one(fixture.database.pool())
                .await?;
            let receipts: i64 =
                sqlx::query_scalar("SELECT count(*) FROM durable_operation_receipts")
                    .fetch_one(fixture.database.pool())
                    .await?;
            ensure!(
                links == 0 && receipts == 0,
                "gender conflict left a durable association or receipt"
            );
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_profile_and_canonical_genders_conflict_then_durable_link_is_rejected() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            sqlx::query("UPDATE athlete_sources SET gender='M' WHERE id=$1")
                .bind(fixture.athlete_source_id)
                .execute(fixture.database.pool())
                .await?;
            let (workspace, spec) = manifest_with_gender(&fixture, test, "M")?;
            ensure!(
                apply(&fixture, &workspace, &spec).await.is_err(),
                "profile gender conflicting with the canonical athlete was accepted"
            );
            let links: i64 = sqlx::query_scalar("SELECT count(*) FROM reviewed_result_links")
                .fetch_one(fixture.database.pool())
                .await?;
            ensure!(
                links == 0,
                "profile/canonical gender conflict left a durable association"
            );
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_result_facts_change_after_review_then_claim_is_not_exported_and_review_issue_is_visible()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let (workspace, _artifact, spec) = manifest(&fixture, test)?;
            apply(&fixture, &workspace, &spec).await?;
            sqlx::query("UPDATE results SET raw_mark='23:04.4' WHERE id=$1")
                .bind(fixture.result_row_id)
                .execute(fixture.database.pool())
                .await?;
            let (records, issues) = fixture
                .database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            let target = records
                .iter()
                .find(|record| record.id == fixture.athlete_id)
                .context("reviewed target missing")?;
            ensure!(
                target.performances.is_empty(),
                "stale reviewed result remained attributed to the target"
            );
            ensure!(
                issues.iter().any(|issue| issue
                    .detail
                    .contains("reviewed result link invalidated: result_snapshot_changed")),
                "stale association was not visible in Review issues"
            );
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_target_identity_changes_after_review_then_claim_is_invalidated() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let (workspace, _artifact, spec) = manifest(&fixture, test)?;
            apply(&fixture, &workspace, &spec).await?;
            sqlx::query("UPDATE athletes SET name='Audrey Koehler Changed' WHERE id=$1")
                .bind(fixture.athlete_id)
                .execute(fixture.database.pool())
                .await?;
            let (records, issues) = fixture
                .database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            let target = records
                .iter()
                .find(|record| record.id == fixture.athlete_id)
                .context("reviewed target missing")?;
            ensure!(
                target.performances.is_empty(),
                "stale identity claim remained attributed"
            );
            ensure!(
                issues
                    .iter()
                    .any(|issue| issue.detail.contains("target_snapshot_changed")),
                "changed target identity was not surfaced for review"
            );
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_physical_owner_appears_after_review_then_physical_owner_wins_without_duplicate()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let (workspace, _artifact, spec) = manifest(&fixture, test)?;
            apply(&fixture, &workspace, &spec).await?;
            let other_id: i64 = sqlx::query_scalar(
                "INSERT INTO athletes(name,gender,graduation_year,state) VALUES('Other Runner','F',2027,'WI') RETURNING id",
            )
            .fetch_one(fixture.database.pool())
            .await?;
            let other_source_id: i64 = sqlx::query_scalar(
                "INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,name,school_id,gender,graduation_year) \
                 VALUES($1,'milesplit','999999','https://wi.milesplit.com/athletes/999999-other-runner','Other Runner',$2,'F',2027) RETURNING id",
            )
            .bind(other_id)
            .bind(fixture.school_id)
            .fetch_one(fixture.database.pool())
            .await?;
            sqlx::query(
                "UPDATE results SET athlete_source_id=$2,athlete_id=$3,raw=raw || jsonb_build_object( \
                   'athlete_id','999999','event',event,'mark',raw_mark,'sport','cross_country', \
                   'result_id',source_result_id,'is_personal_record',is_personal_record,'is_relay',is_relay) \
                 WHERE id=$1",
            )
                .bind(fixture.result_row_id)
                .bind(other_source_id)
                .bind(other_id)
                .execute(fixture.database.pool())
                .await?;
            let (records, issues) = fixture.database.export_snapshot(&["WI".to_owned()], &[2027]).await?;
            let prior_target = records.iter().find(|record| record.id == fixture.athlete_id).context("reviewed target missing")?;
            let physical_target = records.iter().find(|record| record.id == other_id).context("physical owner missing")?;
            ensure!(prior_target.performances.is_empty(), "reviewed association overrode a physical result owner");
            ensure!(physical_target.performances.len() == 1, "physical result was not exported under its database owner exactly once");
            ensure!(issues.iter().any(|issue| issue.detail.contains("physical_owner_present")), "invalidated reviewed association was not visible");
            fixture.database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_saved_school_address_conflicts_then_generic_review_export_includes_bounded_json()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            sqlx::query(
                "INSERT INTO schools(source,source_id,name,state,source_url,address_provenance) \
                 VALUES('milesplit','fall-creek-review','Fall Creek','WI','https://wi.milesplit.com/teams/1', \
                   jsonb_build_object('manual_research_conflicts',jsonb_build_array(jsonb_build_object( \
                     'needs_review',true,'field','address','reviewed_value','1 Main St','incoming_value','2 Main St', \
                     'incoming_source_url','https://wi.milesplit.com/teams/1'))))",
            )
            .execute(database.pool())
            .await?;
            let (records, issues) = database.export_snapshot(&["WI".to_owned()], &[2027]).await?;
            ensure!(records.is_empty(), "school-only review fixture unexpectedly exported athletes");
            let issue = issues.iter().find(|issue| issue.detail.contains("manual research address conflict"))
                .context("saved address conflict did not reach generic Review export")?;
            ensure!(issue.detail.contains("fall-creek-review") && issue.detail.contains("address") && issue.detail.contains("reviewed_value"), "conflict diagnostic omitted its bounded identity or field values");
            ensure!(issue.url == "https://wi.milesplit.com/teams/1", "conflict Review issue lost its public evidence URL");
            database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_tampered_evidence_artifact_then_prepare_rejects_before_database_write() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let (workspace, artifact, _spec) = manifest(&fixture, test)?;
            std::fs::write(&artifact.path, b"tampered")?;
            ensure!(
                reviewed_result_links::prepare(&workspace, &artifact).is_err(),
                "tampered manifest was accepted"
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM reviewed_result_links")
                .fetch_one(fixture.database.pool())
                .await?;
            ensure!(count == 0, "prepare wrote association rows");
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_mixed_manifest_with_conflicting_target_then_durable_transaction_rolls_back_all_links()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let second_id = "raw:[\"790295\",\"milesplit-file:790295:1336298\",\"Girls 5000 Meter\",null,153,\"f95210eef37ca2ea95e60d546d22bfb62fa9804a639ed6016290005c24fcdd52\"]";
            let second_html = "  20 Audrey Koehler            SR Fall Creek                                 23:45.5        ";
            let second = UnassignedResult {
                result_id: Some(second_id.to_owned()),
                source_row: None,
                event: "Girls 5000 Meter".to_owned(),
                mark: "23:45.5".to_owned(),
                sport: Sport::CrossCountry,
                school_id: None,
                meet_id: Some("790295".to_owned()),
                date: Some("2026-10-24".to_owned()),
                is_relay: false,
                members: Vec::new(),
                raw: json!({
                    "kind":"raw_result",
                    "name":"Audrey Koehler",
                    "team":"Fall Creek",
                    "grade":"SR",
                    "published_file_identity":FILE_IDENTITY,
                    "line_ordinal":153,
                    "published_html":second_html,
                    "meet_name":"Fall Creek Invitational"
                }),
            };
            let combined = format!("{PUBLISHED_HTML}\\n{second_html}");
            let capture = Capture {
                url: fixture.result_target.url.clone(),
                status: 200,
                content_type: "text/html".to_owned(),
                body: combined.as_bytes().to_vec(),
            };
            let page = ParsedPage {
                unassigned_results: vec![fixture.result.clone(), second],
                ..ParsedPage::default()
            };
            fixture.database.persist(None, &fixture.result_target, &capture, &page).await?;
            let second_key = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(Source::MileSplit, "result", second_id))?)
            );
            ensure!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM results WHERE source_result_key=ANY($1)")
                    .bind(vec![fixture.result_key.clone(), second_key])
                    .fetch_one(fixture.database.pool()).await? == 2,
                "conflict fixture did not create two source observations"
            );

            let (workspace, artifact, _) = manifest(&fixture, test)?;
            let mut document: Value = serde_json::from_slice(&workspace.read(&artifact)?)?;
            let mut second_change = document["changes"][0].clone();
            second_change["source_result_key"] = json!(format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(Source::MileSplit, "result", second_id))?)
            ));
            second_change["source_result_id"] = json!(second_id);
            second_change["line_ordinal"] = json!(153);
            second_change["published_html"] = json!(second_html);
            second_change["mark"] = json!("23:45.5");
            second_change["target_athlete_id"] = json!(fixture.athlete_id + 1000);
            document["changes"].as_array_mut().context("manifest changes")?.push(second_change);

            let root = test.directory().join("reviewed-result-artifacts");
            let evidence = staged_capture(&root, "combined-results.txt", combined.as_bytes())?;
            document["evidence"][0]["artifact"] = serde_json::to_value(evidence)?;
            let manifest_path = test.directory().join("reviewed-result-conflict.json");
            std::fs::write(&manifest_path, serde_json::to_vec(&document)?)?;
            let manifest_artifact = stage_input(&root, &manifest_path)?;
            let spec = OperationSpec::ReviewedResultLinks {
                manifest: manifest_artifact.clone(),
            };
            let conflict_workspace = Workspace::open(&root, "reviewed-result-conflict", &spec)?;
            ensure!(
                track_scrapper_v2::durable::operations::execute_import(
                    &conflict_workspace,
                    &spec,
                    &fixture.database_url,
                )
                .await
                .is_err(),
                "mixed valid/conflicting manifest unexpectedly committed"
            );
            let links: i64 = sqlx::query_scalar("SELECT count(*) FROM reviewed_result_links")
                .fetch_one(fixture.database.pool())
                .await?;
            let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_operation_receipts")
                .fetch_one(fixture.database.pool())
                .await?;
            ensure!(links == 0 && receipts == 0, "failed durable import left a partial association or receipt");
            fixture.database.close().await;
            Ok(())
        })
    }).await
}
