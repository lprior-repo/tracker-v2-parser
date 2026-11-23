#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::page::{self, PageOutcome},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
    sources,
};

const RAW: &str = include_str!("../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html");
const REQUESTED: &str = "https://oh.milesplit.com/meets/770621/results/1321880/raw";
const ALIAS: &str =
    "https://oh.milesplit.com/meets/770621-beaver-eastern-invite-2026/results/1321880/raw";
const FINAL: &str = "https://www.milesplit.com/meets/770621/results/1321880/raw";
const LATER_FINAL: &str =
    "https://www.milesplit.com/meets/770621-beaver-eastern-invite-2026/results/1321880/raw";

type Issue = (String, String, String);

fn target(url: &str, acquisition_state: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Results,
        url.into(),
        PageContext {
            state: Some("OH".into()),
            acquisition_state: Some(acquisition_state.into()),
            sport: Some(Sport::CrossCountry),
            ..PageContext::default()
        },
    )
}

fn capture(url: &str) -> Capture {
    Capture {
        url: url.into(),
        status: 200,
        content_type: "text/html".into(),
        body: RAW.as_bytes().to_vec(),
    }
}

async fn unassigned(database: &Database, states: &[&str]) -> Result<Vec<Issue>> {
    let states = states
        .iter()
        .map(|state| (*state).to_owned())
        .collect::<Vec<_>>();
    let (athletes, issues) = database.export_snapshot(&states, &[2028]).await?;
    assert!(athletes.is_empty());
    Ok(issues
        .into_iter()
        .filter(|issue| issue.detail.starts_with("unassigned result | "))
        .map(|issue| (issue.source, issue.url, issue.detail))
        .collect())
}

async fn observation_state(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object(\
         'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),\
         'pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),\
         'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY operation_id) FROM durable_page_operations o),\
         'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a),\
         'receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_id) FROM durable_page_receipts r),\
         'requests',(SELECT count(*) FROM durable_wire_requests))",
    )
    .fetch_one(database.pool())
    .await?)
}

async fn assert_scopes(
    database: &Database,
    expected_states: &[&str],
    final_url: &str,
) -> Result<()> {
    let before = observation_state(database).await?;
    assert_eq!(before["requests"], 0);
    let expected = unassigned(database, &[]).await?;
    assert_eq!(expected.len(), 80);
    assert_eq!(
        expected.first().context("first retained raw observation")?,
        &(
            "milesplit".into(),
            final_url.into(),
            "unassigned result | Boys Middle School 3000 Meter | 12:40.6 | unclassified".into(),
        )
    );
    for state in expected_states {
        let actual = unassigned(database, &[*state]).await?;
        assert_eq!(
            actual.len(),
            expected.len(),
            "acquisition state {state} lost raw observations"
        );
        assert_eq!(actual, expected);
    }
    let mut combined = expected_states.to_vec();
    combined.extend(["OH", " oh ", "MI"]);
    assert_eq!(unassigned(database, &combined).await?, expected);
    let (_, unrelated) = database.export_snapshot(&["MI".into()], &[2028]).await?;
    assert!(unrelated.is_empty());
    assert_eq!(observation_state(database).await?, before);
    Ok(())
}

async fn unrelated_checkpoint(database: &Database) -> Result<()> {
    let mut request = target(
        "https://mi.milesplit.com/meets/999999/results/999999/raw",
        "MI",
    );
    request.context.state = Some("MI".into());
    let capture = Capture {
        url: request.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: b"<html></html>".to_vec(),
    };
    database
        .persist(None, &request, &capture, &ParsedPage::default())
        .await?;
    Ok(())
}

async fn library_capture(test: &support::Scenario, final_url: &str) -> Result<()> {
    let database = Database::connect(&test.url).await?;
    unrelated_checkpoint(&database).await?;
    let request = target(REQUESTED, "OH");
    let parsed = sources::parse(&request, RAW)?;
    assert_eq!(parsed.unassigned_results.len(), 80);
    assert!(parsed.athletes.is_empty() && parsed.performances.is_empty());
    assert!(
        parsed
            .unassigned_results
            .iter()
            .all(|row| row.school_id.is_none())
    );
    let checkpoint = database
        .persist(None, &request, &capture(final_url), &parsed)
        .await?;
    let stored: (String, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT url FROM scrape_pages WHERE id=$1),\
         (SELECT count(*) FROM results WHERE source_url=$2),\
         (SELECT count(*) FROM results WHERE raw#>>'{raw,source_url}'=$3),\
         (SELECT count(*) FROM durable_page_receipts)",
    )
    .bind(checkpoint)
    .bind(final_url)
    .bind(REQUESTED)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored, (REQUESTED.into(), 80, 80, 0));
    assert_scopes(&database, &["OH"], final_url).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn given_direct_raw_capture_when_exporting_acquisition_state_then_each_observation_is_retained_once()
-> Result<()> {
    support::with_schema(|test| Box::pin(library_capture(test, REQUESTED))).await
}

#[tokio::test]
async fn given_redirected_raw_capture_when_exporting_acquisition_state_then_each_observation_is_retained_once()
-> Result<()> {
    support::with_schema(|test| Box::pin(library_capture(test, FINAL))).await
}

#[tokio::test]
async fn given_durable_alias_recapture_when_raw_request_url_changes_then_prior_receipt_keeps_acquisition_scope()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            unrelated_checkpoint(&database).await?;
            let config = Config::parse("states=['OH','WI']\ngraduation_years=[2028]\nseason=2026\n")?;
            let scope = page_support::scope(&config)?;
            let mut later = scope.clone();
            later.run_generation = "later-redirected-recapture".into();
            let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
            let root = test.directory().join("redirected-results");
            for (run, request, final_url) in [
                (&scope, target(REQUESTED, "OH"), FINAL),
                (&later, target(REQUESTED, "OH"), LATER_FINAL),
                (&scope, target(ALIAS, "WI"), FINAL),
            ] {
                let outcome = page::offline_replay(&database, track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking), run, request, capture(final_url)).await?;
                assert!(matches!(outcome, PageOutcome::Completed { .. }));
            }
            let facts: (i64, i64, i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM results),\
                 (SELECT count(*) FROM results WHERE source_url=$1 AND raw#>>'{raw,source_url}'=$2),\
                 (SELECT count(*) FROM durable_page_receipts WHERE capture->>'url'=$1),\
                 (SELECT count(*) FROM scrape_pages WHERE url=$1),\
                 (SELECT count(*) FROM durable_page_receipts)",
            )
            .bind(FINAL)
            .bind(ALIAS)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(facts, (80, 80, 2, 0, 3));
            assert_scopes(&database, &["OH", "WI"], FINAL).await?;
            let before = observation_state(&database).await?;
            let replay = page::offline_replay(&database, track_scrapper_v2::durable::page::PageFiles::new(&root, &blocking), &scope, target(REQUESTED, "OH"), capture(FINAL)).await?;
            assert!(matches!(replay, PageOutcome::Completed { .. }));
            assert_eq!(observation_state(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}
