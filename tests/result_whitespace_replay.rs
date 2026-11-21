#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target, UnassignedResult},
    sources::athleticnet,
};

const WHITESPACE: [&str; 6] = [
    " ",
    "\t",
    "\r\n",
    "\u{00a0}",
    "\u{2003}",
    "\u{0009}\u{000a}\u{000b}\u{000c}\u{000d}\u{0020}\u{0085}\u{00a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}",
];

fn evidence(value: &str) -> Value {
    json!({
        "published":value,
        "eventType":{"label":value},
        "row":{"label":value,"attributes":{"data-level":value}},
        "context":{"meet_name":value}
    })
}

fn published_page(mark: &str, status: Option<&str>) -> ParsedPage {
    let mut assigned = records::performance(mark);
    assigned.source_row = Some("published-assigned-row".into());
    assigned.course = Some("Published Course".into());
    assigned.reported_status = status.map(str::to_owned);
    let mut values = evidence("published");
    let mut raw = json!({"source_values":evidence("published")});
    if let Some(status) = status {
        values["statusCode"] = json!(status);
        raw["statusCode"] = json!(status);
    }
    assigned.source_values = Some(values);
    ParsedPage {
        athletes: vec![records::athlete("11")],
        performances: vec![assigned],
        unassigned_results: vec![UnassignedResult {
            result_id: Some("unassigned-whitespace".into()),
            source_row: Some("published-unassigned-row".into()),
            event: "100 Meters".into(),
            mark: mark.into(),
            sport: Sport::Outdoor,
            school_id: None,
            meet_id: Some("meet-1".into()),
            date: Some("2026-05-15".into()),
            is_relay: false,
            members: Vec::new(),
            raw,
        }],
        ..ParsedPage::default()
    }
}

fn sparse_page(whitespace: &str) -> ParsedPage {
    let mut page = published_page("12.34", None);
    let assigned = &mut page.performances[0];
    assigned.event = whitespace.into();
    assigned.mark = whitespace.into();
    assigned.source_row = Some(whitespace.into());
    assigned.meet_id = Some(whitespace.into());
    assigned.meet_name = Some(whitespace.into());
    assigned.date = Some(whitespace.into());
    assigned.timing = Some(whitespace.into());
    assigned.course = Some(whitespace.into());
    assigned.grade = Some(whitespace.into());
    assigned.gender = Some(whitespace.into());
    assigned.source_values = Some(evidence(whitespace));
    let unassigned = &mut page.unassigned_results[0];
    unassigned.event = whitespace.into();
    unassigned.mark = whitespace.into();
    unassigned.source_row = Some(whitespace.into());
    unassigned.meet_id = Some(whitespace.into());
    unassigned.date = Some(whitespace.into());
    unassigned.raw = json!({"source_values":evidence(whitespace)});
    page
}

async fn snapshot(database: &Database) -> Result<Vec<Value>> {
    Ok(sqlx::query_scalar(
        "SELECT to_jsonb(r)-'last_seen' FROM results r ORDER BY source_result_id",
    )
    .fetch_all(database.pool())
    .await?)
}

fn assert_facts(row: &Value, mark: &str, status: &str, numeric: Option<i64>) {
    assert_eq!(row["event"], "100 Meters");
    assert_eq!(row["raw_mark"], mark);
    assert_eq!(row["raw"]["event"], "100 Meters");
    assert_eq!(row["raw"]["mark"], mark);
    assert_eq!(row["status"], status);
    assert_eq!(row["normalized_numeric"], json!(numeric));
    assert_eq!(row["normalized_unit"], json!(numeric.map(|_| "ns")));
}

#[tokio::test]
async fn given_published_results_when_unicode_blank_fields_replay_then_sql_and_raw_facts_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = records::target(Source::AthleticNet, "11");
            records::persist(&database, &target, &published_page("12.34", None)).await?;
            let original = snapshot(&database).await?;
            assert_eq!(original.len(), 2);
            for whitespace in WHITESPACE {
                assert!(whitespace.trim().is_empty());
                records::persist(&database, &target, &sparse_page(whitespace)).await?;
                assert_eq!(snapshot(&database).await?, original, "{whitespace:?}");
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_failure_results_when_unicode_blank_marks_replay_then_failures_survive_until_correction()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = records::target(Source::AthleticNet, "11");
            records::persist(&database, &target, &published_page("12.34", Some("DQ"))).await?;
            let original = snapshot(&database).await?;
            assert_eq!(original.len(), 2);
            for row in &original {
                assert_facts(row, "12.34", "DQ", None);
            }
            for whitespace in WHITESPACE {
                records::persist(&database, &target, &sparse_page(whitespace)).await?;
                assert_eq!(snapshot(&database).await?, original, "{whitespace:?}");
            }
            let correction = published_page(" 12.35\t", None);
            for _ in 0..2 {
                records::persist(&database, &target, &correction).await?;
            }
            let corrected = snapshot(&database).await?;
            assert_eq!(corrected.len(), 2);
            for row in corrected {
                assert_facts(&row, " 12.35\t", "recorded", Some(12_350_000_000));
                assert!(row["raw"].get("reported_status").is_none());
                assert!(row["raw"]["source_values"].get("statusCode").is_none());
                assert!(row["raw"]["raw"].get("statusCode").is_none());
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn persist_unidentified(database: &Database, target: &Target, mark: &str) -> Result<()> {
    let body = json!({"teams":[],"flatEvents":[{
        "Event":"100 Meters","isPersonal":true,
        "results":[{"IDResult":51,"Result":mark}]
    }]})
    .to_string();
    let page = athleticnet::parse(target, &body)?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert_eq!(page.unassigned_results.len(), 1);
    assert_eq!(page.unassigned_results[0].result_id.as_deref(), Some("51"));
    assert_eq!(page.unassigned_results[0].mark, mark);
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".into(),
        body: body.into_bytes(),
    };
    database.persist(None, target, &capture, &page).await?;
    Ok(())
}

#[tokio::test]
async fn given_unidentified_athleticnet_result_when_parser_retains_blank_replay_then_mark_is_preserved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(
                Source::AthleticNet,
                PageKind::Results,
                "https://www.athletic.net/api/v1/Meet/GetAllResultsData?meetId=41&sport=tf&rawResults=false&showTips=false".into(),
                PageContext {
                    sport: Some(Sport::Outdoor),
                    subject_id: Some("41".into()),
                    meet_date: Some("2026-05-15".into()),
                    ..PageContext::default()
                },
            );
            for mark in ["12.34", "\t", "\u{00a0}"] {
                persist_unidentified(&database, &target, mark).await?;
                let rows = snapshot(&database).await?;
                assert_eq!(rows.len(), 1);
                let row = &rows[0];
                assert_facts(row, "12.34", "recorded", Some(12_340_000_000));
                assert_eq!(row["raw"]["raw"]["Result"], mark);
                assert_eq!(row["athlete_id"], Value::Null);
                assert_eq!(row["athlete_source_id"], Value::Null);
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}
