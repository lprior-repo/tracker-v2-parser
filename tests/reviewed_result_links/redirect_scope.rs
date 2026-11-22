use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::model::{Capture, PageContext, PageKind, ParsedPage, Source, Target};

use super::{Fixture, PUBLISHED_HTML, RESULT_URL, apply, fixture, manifest, support};

const REQUESTED: &str = "https://oh.milesplit.com/meets/790295/results/1336298/raw";

async fn recapture_redirect(fixture: &Fixture) -> Result<()> {
    let mut result = fixture.result.clone();
    result.raw["source_url"] = json!(REQUESTED);
    let target = Target::get(
        Source::MileSplit,
        PageKind::Results,
        REQUESTED.into(),
        PageContext {
            state: Some("OH".into()),
            acquisition_state: Some("OH".into()),
            ..PageContext::default()
        },
    );
    let capture = Capture {
        url: RESULT_URL.into(),
        status: 200,
        content_type: "text/html".into(),
        body: PUBLISHED_HTML.as_bytes().to_vec(),
    };
    let page = ParsedPage {
        unassigned_results: vec![result],
        ..ParsedPage::default()
    };
    fixture
        .database
        .persist(None, &target, &capture, &page)
        .await?;
    Ok(())
}

async fn observation_state(fixture: &Fixture) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object(\
         'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),\
         'pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),\
         'links',(SELECT jsonb_agg(to_jsonb(l) ORDER BY source_result_key) FROM reviewed_result_links l),\
         'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY operation_id) FROM durable_page_operations o),\
         'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a),\
         'page_receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_id) FROM durable_page_receipts r),\
         'operation_receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_key) FROM durable_operation_receipts r),\
         'requests',(SELECT count(*) FROM durable_wire_requests))",
    )
    .fetch_one(fixture.database.pool())
    .await?)
}

#[tokio::test]
async fn given_reviewed_raw_result_recaptured_through_redirect_when_exporting_acquisition_state_then_invalid_link_is_retained_once()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            let (workspace, _, spec) = manifest(&fixture, test)?;
            assert_eq!(apply(&fixture, &workspace, &spec).await?["links_added"], 1);
            recapture_redirect(&fixture).await?;
            let reason: String = sqlx::query_scalar(
                "SELECT invalidation_reason FROM invalid_reviewed_result_links WHERE source_result_key=$1",
            )
            .bind(&fixture.result_key)
            .fetch_one(fixture.database.pool())
            .await?;
            assert_eq!(reason, "result_snapshot_changed");
            let before = observation_state(&fixture).await?;
            assert_eq!(before["requests"], 0);
            for states in [vec!["OH".into()], vec!["OH".into(), " oh ".into(), "MI".into()]] {
                let (athletes, issues) = fixture.database.export_snapshot(&states, &[2027]).await?;
                assert!(athletes.is_empty());
                assert_eq!(issues.len(), 1);
                let issue = issues.first().context("redirected reviewed result issue")?;
                assert_eq!(issue.source, "milesplit");
                assert_eq!(issue.url, RESULT_URL);
                assert_eq!(issue.detail, "reviewed result link invalidated: result_snapshot_changed | Girls 5000 Meter | 23:03.4 | recorded");
            }
            assert!(fixture.database.export_snapshot(&["MI".into()], &[2027]).await?.1.is_empty());
            assert_eq!(observation_state(&fixture).await?, before);
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}
