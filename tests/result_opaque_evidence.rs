mod support;

use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target, UnassignedResult},
};

fn meet() -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Results,
        "https://www.athletic.net/api/v1/Meet/GetAllResultsData?meetId=41".into(),
        PageContext::default(),
    )
}

async fn persist(database: &Database, mark: &str, raw: Value) -> Result<()> {
    let target = meet();
    let page = ParsedPage {
        unassigned_results: vec![UnassignedResult {
            result_id: Some("51".into()),
            source_row: Some("retained-row".into()),
            event: "100 Meters".into(),
            mark: mark.into(),
            sport: Sport::Outdoor,
            meet_id: Some("41".into()),
            date: Some("2026-05-15".into()),
            school_id: None,
            is_relay: false,
            members: Vec::new(),
            raw,
        }],
        ..ParsedPage::default()
    };
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".into(),
        body: serde_json::to_vec(&page)?,
    };
    database.persist(None, &target, &capture, &page).await?;
    Ok(())
}

async fn rows(database: &Database) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(r)-'last_seen' FROM results r ORDER BY id")
            .fetch_all(database.pool())
            .await?,
    )
}

async fn assert_replay_preserves(evidence: Vec<Value>) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for value in evidence {
                persist(&database, "12.34", value.clone()).await?;
                let original = rows(&database).await?;
                assert_eq!(original.len(), 1);
                assert_eq!(original[0]["raw"].get("raw"), Some(&value));
                assert_eq!(original[0]["source_result_id"], "51");
                assert_eq!(original[0]["normalized_numeric"], 12_340_000_000_i64);
                for mark in ["12.34", "12.34", "\u{00a0}"] {
                    persist(&database, mark, value.clone()).await?;
                    assert_eq!(rows(&database).await?, original);
                }
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_json_null_when_replayed_then_retained_evidence_is_still_json_null() -> Result<()> {
    assert_replay_preserves(vec![Value::Null]).await
}

#[tokio::test]
async fn given_scalar_and_array_when_replayed_then_retained_evidence_does_not_grow() -> Result<()> {
    assert_replay_preserves(vec![
        json!(17),
        json!(true),
        json!("published"),
        json!([null, {"published": 17}]),
    ])
    .await
}

#[tokio::test]
async fn given_opaque_evidence_when_an_object_corrects_it_then_the_same_result_recovers()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for value in [Value::Null, json!("old"), json!(["old"])] {
                persist(&database, "12.34", value).await?;
                let previous = rows(&database).await?;
                let corrected = json!({"source_values":{"row":{"published":true}}});
                persist(&database, "12.35", corrected.clone()).await?;
                let current = rows(&database).await?;
                assert_eq!(current.len(), 1);
                assert_eq!(current[0]["id"], previous[0]["id"]);
                assert_eq!(current[0]["raw"]["raw"], corrected);
                assert_eq!(current[0]["raw_mark"], "12.35");
                assert_eq!(current[0]["normalized_numeric"], 12_350_000_000_i64);
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_published_nested_object_when_sparse_correction_replays_then_known_facts_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            persist(&database, "12.34", json!({"statusCode":"DQ","source_values":{"eventType":{"label":"Dash"},"row":{"attributes":{"data-heat":"1","data-lane":"2"}}}})).await?;
            assert_eq!(rows(&database).await?[0]["status"], "DQ");
            for _ in 0..2 {
                persist(&database, "12.36", json!({"source_values":{"eventType":{"label":"\u{2003}"},"row":{"attributes":{"data-heat":null,"data-lane":"3"}}}})).await?;
            }
            let actual = rows(&database).await?;
            assert_eq!(actual.len(), 1);
            assert_eq!(actual[0]["status"], "recorded");
            assert_eq!(actual[0]["normalized_numeric"], 12_360_000_000_i64);
            assert_eq!(actual[0]["raw"]["raw"], json!({"source_values":{"eventType":{"label":"Dash"},"row":{"attributes":{"data-heat":"1","data-lane":"3"}}}}));
            database.close().await;
            Ok(())
        })
    })
    .await
}
