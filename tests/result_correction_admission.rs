mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources::{athleticnet, milesplit},
};

fn owned_target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Results,
        "https://al.milesplit.com/api/v1/meets/725218/performances?isMeetPro=0".to_owned(),
        PageContext {
            subject_id: Some("725218".to_owned()),
            meet_date: Some("2026-03-27".to_owned()),
            result_set_ids: vec!["1266814".to_owned()],
            sport: Some(Sport::Outdoor),
            ..PageContext::default()
        },
    )
}

fn owned_row() -> Result<Value> {
    let body: Value = serde_json::from_str(include_str!(
        "../fixtures/milesplit/performances-al-725218.json"
    ))?;
    Ok(body["data"][0].clone())
}

async fn replay(database: &Database, target: &Target, body: Value) -> Result<()> {
    let body = body.to_string();
    let page = match target.source {
        Source::MileSplit => milesplit::parse(target, &body)?,
        Source::AthleticNet => athleticnet::parse(target, &body)?,
    };
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".to_owned(),
        body: body.into_bytes(),
    };
    database.persist(None, target, &capture, &page).await?;
    Ok(())
}

fn owned_body(rows: Vec<Value>) -> Value {
    json!({"_meta":{"status_code":200},"data":rows})
}

async fn stored(database: &Database, id: &str) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object('owner', athlete_source_id, 'status', status, \
         'mark', raw_mark, 'numeric', normalized_numeric, 'relay', is_relay, 'raw', raw) \
         FROM results WHERE source_result_id=$1",
    )
    .bind(id)
    .fetch_one(database.pool())
    .await?)
}

async fn count(database: &Database) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM results")
        .fetch_one(database.pool())
        .await?)
}

#[tokio::test]
async fn given_owned_result_when_markless_dq_replayed_then_status_changes_without_duplicate()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let mut row = owned_row()?;
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            let before = stored(&database, "201782806").await?;
            assert!(!before["owner"].is_null());
            assert_eq!(before["numeric"], 12_400_000_000_i64);
            row.as_object_mut().context("publisher row")?.remove("mark");
            row["statusCode"] = json!("DQ");
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            replay(&database, &target, owned_body(vec![row])).await?;
            let after = stored(&database, "201782806").await?;
            assert_eq!(after["owner"], before["owner"]);
            assert_eq!(after["status"], "DQ");
            assert!(after["numeric"].is_null());
            assert_eq!(after["mark"], "12.40");
            assert_eq!(count(&database).await?, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_assigned_row_when_duplicate_relay_replayed_then_rejected_evidence_cannot_demote()
-> Result<()> {
    duplicate_relay(false).await?;
    duplicate_relay(true).await
}

async fn duplicate_relay(reverse: bool) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let row = owned_row()?;
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            let before = stored(&database, "201782806").await?;
            let mut rejected = row.clone();
            rejected["eventName"] = json!("4x100 Meter Relay");
            let body = owned_body(if reverse {
                vec![rejected.clone(), row]
            } else {
                vec![row, rejected.clone()]
            });
            replay(&database, &target, body.clone()).await?;
            replay(&database, &target, body).await?;
            assert_eq!(stored(&database, "201782806").await?, before);
            assert_eq!(count(&database).await?, 3);
            let retained: Vec<Value> =
                sqlx::query_scalar("SELECT raw->'raw' FROM results WHERE source_result_id IS NULL")
                    .fetch_all(database.pool())
                    .await?;
            assert_eq!(retained.len(), 2);
            assert!(retained.contains(&rejected));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_assigned_row_when_wrong_bound_relay_replayed_then_trusted_result_is_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let mut row = owned_row()?;
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            let before = stored(&database, "201782806").await?;
            row["eventName"] = json!("4x100 Meter Relay");
            row["meetId"] = json!("9");
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            replay(&database, &target, owned_body(vec![row])).await?;
            assert_eq!(stored(&database, "201782806").await?, before);
            assert_eq!(count(&database).await?, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn relay_body() -> Value {
    json!({"teams":[],"flatEvents":[{"Event":"4x100 Relay","Gender":"M",
        "isPersonal":false,"results":[{"IDResult":1,"AthleteID":999,
        "FirstName":"Team A","Result":"42.00a","TeamID":3204,
        "SchoolName":"Middleton"}]}],"relayLegs":[
        {"ResultID":1,"AthleteID":42,"Name":"Public Runner","ShortDesc":"11"},
        {"ResultID":1,"AthleteID":43,"Name":"Second Runner","ShortDesc":"11"}]})
}

#[tokio::test]
async fn given_native_relay_members_when_legs_omitted_then_published_members_survive() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let metadata = athleticnet::parse(
                &athleticnet::meet_target("634313", Sport::Outdoor)?,
                include_str!("../fixtures/athleticnet/meet-634313.json"),
            )?;
            let target = metadata.next.first().context("native results target")?;
            let mut body = relay_body();
            replay(&database, target, body.clone()).await?;
            let before = stored(&database, "1").await?;
            assert_eq!(
                before["raw"]["members"],
                json!(["athleticnet:42", "athleticnet:43"])
            );
            body.as_object_mut()
                .context("native response")?
                .remove("relayLegs");
            let sparse = athleticnet::parse(target, &body.to_string())?;
            assert!(sparse.unassigned_results[0].members.is_empty());
            assert!(sparse.unassigned_results[0].raw["relayLegs"].is_null());
            replay(&database, target, body.clone()).await?;
            replay(&database, target, body).await?;
            assert_eq!(stored(&database, "1").await?, before);
            assert_eq!(count(&database).await?, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_correction_batch_when_later_owner_conflicts_then_entire_batch_rolls_back()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let first = owned_row()?;
            let mut second = first.clone();
            second["id"] = json!("201782807");
            replay(
                &database,
                &target,
                owned_body(vec![first.clone(), second.clone()]),
            )
            .await?;
            let before = stored(&database, "201782806").await?;
            let mut correction = first;
            correction
                .as_object_mut()
                .context("publisher row")?
                .remove("mark");
            correction["statusCode"] = json!("DQ");
            second["athleteId"] = json!("999");
            second["profileUrl"] = json!("https://www.milesplit.com/athletes/999-other-runner");
            let failure = replay(&database, &target, owned_body(vec![correction, second])).await;
            assert!(failure.is_err());
            assert_eq!(stored(&database, "201782806").await?, before);
            assert_eq!(count(&database).await?, 2);
            let rejected_owner: i64 =
                sqlx::query_scalar("SELECT count(*) FROM athlete_sources WHERE source_id='999'")
                    .fetch_one(database.pool())
                    .await?;
            assert_eq!(rejected_owner, 0);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_assigned_result_when_validated_relay_correction_replayed_then_owner_is_removed()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let mut row = owned_row()?;
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            assert!(!stored(&database, "201782806").await?["owner"].is_null());
            row["eventName"] = json!("4x100 Meter Relay");
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            replay(&database, &target, owned_body(vec![row])).await?;
            let after = stored(&database, "201782806").await?;
            assert!(after["owner"].is_null());
            assert!(after["numeric"].is_null());
            assert_eq!(after["relay"], true);
            assert_eq!(count(&database).await?, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_assigned_result_when_relay_claims_different_owner_then_conflict_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let mut row = owned_row()?;
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            let before = stored(&database, "201782806").await?;
            row["eventName"] = json!("4x100 Meter Relay");
            row["athleteId"] = json!("999");
            row["profileUrl"] = json!("https://www.milesplit.com/athletes/999-other-runner");
            let parsed = milesplit::parse(&target, &owned_body(vec![row.clone()]).to_string())?;
            assert_eq!(
                parsed.unassigned_results[0].result_id.as_deref(),
                Some("201782806")
            );
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            replay(&database, &target, owned_body(vec![row.clone()])).await?;
            assert_eq!(stored(&database, "201782806").await?, before);
            assert_eq!(count(&database).await?, 2);
            let retained: Value =
                sqlx::query_scalar("SELECT raw->'raw' FROM results WHERE source_result_id IS NULL")
                    .fetch_one(database.pool())
                    .await?;
            assert_eq!(retained, row);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_markless_publisher_dq_when_numeric_mark_returns_then_current_raw_status_recovers()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = owned_target();
            let original = owned_row()?;
            replay(&database, &target, owned_body(vec![original.clone()])).await?;
            let mut dq = original.clone();
            dq.as_object_mut().context("publisher row")?.remove("mark");
            dq["statusCode"] = json!("DQ");
            replay(&database, &target, owned_body(vec![dq])).await?;
            let failed = stored(&database, "201782806").await?;
            assert_eq!(failed["status"], "DQ");
            assert_eq!(failed["raw"]["reported_status"], "DQ");
            assert_eq!(failed["raw"]["source_values"]["statusCode"], "DQ");
            let mut corrected = original;
            corrected["mark"] = json!("12.41");
            corrected
                .as_object_mut()
                .context("publisher row")?
                .remove("statusCode");
            replay(&database, &target, owned_body(vec![corrected.clone()])).await?;
            replay(&database, &target, owned_body(vec![corrected])).await?;
            let recovered = stored(&database, "201782806").await?;
            assert_eq!(recovered["owner"], failed["owner"]);
            assert_eq!(recovered["status"], "recorded");
            assert_eq!(recovered["numeric"], 12_410_000_000_i64);
            assert!(recovered["raw"].get("reported_status").is_none());
            assert!(
                recovered["raw"]["source_values"]
                    .get("statusCode")
                    .is_none()
            );
            assert_eq!(count(&database).await?, 1);
            database.close().await;
            Ok(())
        })
    })
    .await
}
