mod support;

use anyhow::{Context as _, Result};
use calamine::{Reader as _, open_workbook_auto};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, ParsedPage, Source, Sport, Target},
    sources::athleticnet::{meet_target, parse},
};

fn metadata(level: Option<Value>, mask: Option<Value>) -> Value {
    let mut meet =
        json!({"ID":634313,"Name":"Fixture meet","SeasonID":2026,"MeetDate":"2026-05-15"});
    if let Some(level) = level {
        meet["Level"] = level;
    }
    if let Some(mask) = mask {
        meet["LevelMask"] = mask;
    }
    json!({"sport2":"tfo","meet":meet,"tfDivisions":[]})
}

fn results_target(body: &Value) -> Result<Target> {
    parse(&meet_target("634313", Sport::Outdoor)?, &body.to_string())?
        .next
        .into_iter()
        .next()
        .context("results continuation")
}

fn results() -> Value {
    json!({"teams":[],"flatEvents":[{"Event":"100 Meters","EventId":1,"DivId":1,"results":[
        {"IDResult":123,"AthleteID":1,"FirstName":"Fixture","LastName":"Runner","Result":"12.34a","Grade":11,"IDDiv":1,"TeamID":3204,"SchoolName":"Fixture School"}]}]})
}

fn parsed_level(target: &Target, body: &Value) -> Result<Value> {
    let page = parse(target, &body.to_string())?;
    Ok(page
        .performances
        .first()
        .context("performance")?
        .source_values
        .as_ref()
        .context("evidence")?["level"]
        .clone())
}

#[test]
fn given_retained_level_dictionary_when_read_then_four_explicitly_means_high_school() -> Result<()>
{
    let body: Value =
        serde_json::from_str(include_str!("../fixtures/athleticnet/directory-root.json"))?;
    let divisions = body["divInfo"].as_array().context("division dictionary")?;
    assert!(!divisions.is_empty());
    assert!(
        divisions
            .iter()
            .all(|division| division["DivName"] == "High School" && division["Level"] == 4)
    );
    Ok(())
}

#[test]
fn given_legacy_and_enriched_cursors_when_roundtripped_then_old_serialization_is_unchanged()
-> Result<()> {
    let target = results_target(&metadata(Some(json!(4)), None))?;
    let mut legacy = serde_json::to_value(&target)?;
    legacy
        .as_object_mut()
        .context("target object")?
        .remove("competition_evidence");
    let old: Target = serde_json::from_value(legacy.clone())?;
    assert!(old.competition_evidence.is_none());
    assert_eq!(serde_json::to_value(&old)?, legacy);
    assert_eq!(parsed_level(&old, &results())?, Value::Null);
    let resumed: Target = serde_json::from_str(&serde_json::to_string(&target)?)?;
    assert_eq!(parsed_level(&resumed, &results())?, "high school");
    Ok(())
}

#[test]
fn given_direct_row_level_when_context_is_unknown_then_only_explicit_unconflicted_hs_qualifies()
-> Result<()> {
    let target = results_target(&metadata(None, None))?;
    for (level, expected) in [
        (json!(4), json!("high school")),
        (json!(2), Value::Null),
        (json!("mixed"), Value::Null),
    ] {
        let mut body = results();
        body["flatEvents"][0]["results"][0]["Level"] = level;
        assert_eq!(parsed_level(&target, &body)?, expected);
        body["flatEvents"][0]["Level"] = json!(2);
        assert_eq!(parsed_level(&target, &body)?, Value::Null);
    }
    Ok(())
}

#[test]
fn given_excessive_metadata_when_followed_then_competition_evidence_is_bounded() -> Result<()> {
    let mut body = metadata(Some(json!(4)), None);
    body["tfDivisions"] = json!(vec![json!({"IDDiv":1}); 257]);
    assert!(results_target(&body).is_err());
    body["tfDivisions"] = json!([]);
    body["meet"]["Level"] = json!("x".repeat(65_537));
    assert!(results_target(&body).is_err());
    Ok(())
}

#[test]
fn given_captured_masks_when_resumed_then_raw_masks_survive_without_guessed_eligibility()
-> Result<()> {
    let body: Value =
        serde_json::from_str(include_str!("../fixtures/athleticnet/meet-634313.json"))?;
    let target = results_target(&body)?;
    let resumed: Target = serde_json::from_str(&serde_json::to_string(&target)?)?;
    let evidence = resumed
        .competition_evidence
        .as_ref()
        .context("competition evidence")?;
    assert_eq!(evidence.meet["LevelMask"], 4);
    assert_eq!(evidence.divisions.len(), 2);
    assert!(
        evidence
            .divisions
            .iter()
            .all(|division| division["LevelMask"] == 4)
    );
    let page = parse(
        &resumed,
        include_str!("../fixtures/athleticnet/results-634313.json"),
    )?;
    assert_eq!(page.performances.len(), 686);
    assert!(page.performances.iter().all(|performance| {
        performance
            .source_values
            .as_ref()
            .is_some_and(|values| values["level"].is_null())
    }));
    Ok(())
}

#[test]
fn given_division_evidence_when_ids_or_levels_conflict_then_hs_is_not_inherited() -> Result<()> {
    let mut body = metadata(Some(json!(4)), None);
    body["tfDivisions"] = json!([{"MeetID":634313,"IDDiv":1,"Level":4}]);
    assert_eq!(
        parsed_level(&results_target(&body)?, &results())?,
        "high school"
    );
    for division in [
        json!({"MeetID":634313,"IDDiv":1,"Level":2}),
        json!({"MeetID":634313,"IDDiv":1,"LevelMask":6}),
        json!({"MeetID":1,"IDDiv":1,"Level":4}),
        json!({"MeetID":634313,"IDDiv":2,"Level":4}),
    ] {
        body["tfDivisions"] = json!([division]);
        assert_eq!(
            parsed_level(&results_target(&body)?, &results())?,
            Value::Null
        );
    }
    body["tfDivisions"] =
        json!([{"MeetID":634313,"IDDiv":1,"Level":4},{"MeetID":634313,"IDDiv":1,"Level":2}]);
    assert_eq!(
        parsed_level(&results_target(&body)?, &results())?,
        Value::Null
    );
    let target = results_target(&metadata(Some(json!(4)), None))?;
    for mismatched in ["source", "meet_id", "sport"] {
        let mut encoded = serde_json::to_value(&target)?;
        encoded["competition_evidence"][mismatched] = match mismatched {
            "source" => json!("milesplit"),
            "sport" => json!("indoor"),
            _ => json!("1"),
        };
        assert_eq!(
            parsed_level(&serde_json::from_value(encoded)?, &results())?,
            Value::Null
        );
    }
    Ok(())
}

async fn persist(
    database: &Database,
    job: Option<i64>,
    target: &Target,
    body: &str,
    page: &ParsedPage,
) -> Result<()> {
    database
        .persist(
            job,
            target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".into(),
                body: body.as_bytes().to_vec(),
            },
            page,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn given_old_exhausted_cursor_when_evidence_arrives_then_key_and_attempts_are_preserved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let enriched = results_target(&metadata(Some(json!(4)), None))?;
            let mut old = enriched.clone();
            old.competition_evidence = None;
            database.enqueue(&[old.clone()], false).await?;
            let job = database.next_job(None, 3, &[]).await?.context("old job")?;
            let expected_key = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(
                    old.source,
                    old.kind,
                    &old.url,
                    "GET",
                    &old.body,
                    &old.context
                ))?)
            );
            let key: String = sqlx::query_scalar("SELECT page_key FROM scrape_pages WHERE id=$1")
                .bind(job.id)
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(key, expected_key);
            for _ in 0..3 {
                assert!(database.start_attempt(job.id, 3).await?);
                database
                    .fail_job(&job, None, "fixture failure", false)
                    .await?;
            }
            for refresh in [false, true] {
                assert_eq!(database.enqueue(&[enriched.clone()], refresh).await?, 0);
                let row: (String, i32, String, Value) = sqlx::query_as(
                    "SELECT status,attempts,page_key,cursor FROM scrape_pages WHERE id=$1",
                )
                .bind(job.id)
                .fetch_one(&test.pool)
                .await?;
                assert_eq!((row.0.as_str(), row.1, row.2), ("failed", 3, key.clone()));
                assert!(row.3.get("competition_evidence").is_none());
            }
            assert!(database.next_job(None, 3, &[]).await?.is_none());
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_completed_old_cursor_when_explicitly_refreshed_then_evidence_updates_same_job()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let enriched = results_target(&metadata(Some(json!(4)), None))?;
            let mut old = enriched.clone();
            old.competition_evidence = None;
            database.enqueue(&[old], false).await?;
            let job = database.next_job(None, 3, &[]).await?.context("old job")?;
            assert!(database.start_attempt(job.id, 3).await?);
            persist(
                &database,
                Some(job.id),
                &job.target,
                "{}",
                &ParsedPage::default(),
            )
            .await?;
            assert_eq!(database.enqueue(&[enriched.clone()], false).await?, 0);
            assert_eq!(database.enqueue(&[enriched], true).await?, 1);
            let resumed = database
                .next_job(None, 3, &[])
                .await?
                .context("refreshed job")?;
            assert_eq!(resumed.id, job.id);
            assert_eq!(parsed_level(&resumed.target, &results())?, "high school");
            let row: (i64, i32, String) = sqlx::query_as(
                "SELECT count(*) OVER(),attempts,status FROM scrape_pages WHERE id=$1",
            )
            .bind(job.id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(row, (1, 0, "pending".into()));
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn export_case(
    test: &support::Scenario,
    level: Option<Value>,
    mask: Option<Value>,
    eligible: bool,
) -> Result<()> {
    let database = Database::connect(&test.url).await?;
    let target = results_target(&metadata(level, mask))?;
    let body = results().to_string();
    let page = parse(&target, &body)?;
    persist(&database, None, &target, &body, &page).await?;
    persist(&database, None, &target, &body, &page).await?;
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT raw_mark,raw->'source_values'->>'level' FROM results")
            .fetch_all(&test.pool)
            .await?;
    assert_eq!(
        rows,
        vec![("12.34a".into(), eligible.then(|| "high school".into()))]
    );
    database.close().await;
    let output = test.directory().join("fixture.xlsx");
    support::assert_success(
        &test
            .cli(&[
                "export",
                "--output",
                output.to_str().context("output path")?,
            ])
            .await?,
    );
    let mut workbook = open_workbook_auto(&output)?;
    let athletes = workbook.worksheet_range("Athletes")?;
    let headers = athletes.rows().next().context("athlete headers")?;
    assert_eq!(
        headers
            .iter()
            .any(|cell| cell.to_string().starts_with("best_observed::athleticnet::")),
        eligible
    );
    let observations = workbook.worksheet_range("Observations")?;
    assert_eq!(observations.rows().count(), 2);
    assert!(
        observations
            .rows()
            .nth(1)
            .context("observation")?
            .iter()
            .any(|cell| cell.to_string() == "12.34a")
    );
    Ok(())
}

#[tokio::test]
async fn given_explicit_and_unknown_levels_when_persisted_twice_and_exported_then_only_hs_qualifies()
-> Result<()> {
    for (level, mask, eligible) in [
        (Some(json!(4)), None, true),
        (Some(json!("High School")), None, true),
        (Some(json!(2)), None, false),
        (None, Some(json!(6)), false),
        (None, Some(json!(4)), false),
        (None, None, false),
    ] {
        support::with_schema(|test| Box::pin(export_case(test, level, mask, eligible))).await?;
    }
    Ok(())
}
