use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::model::{
    Athlete, Capture, PageContext, PageKind, ParsedPage, School, Source, Target,
};

use super::{Fixture, PROFILE_URL, RESULT_URL, apply, fixture, manifest, support};

async fn retain_acquisition_scope(fixture: &Fixture) -> Result<()> {
    let mut target = fixture.result_target.clone();
    target.context.acquisition_state = Some("WI".into());
    let page = ParsedPage {
        unassigned_results: vec![fixture.result.clone()],
        ..ParsedPage::default()
    };
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: super::PUBLISHED_HTML.as_bytes().to_vec(),
    };
    fixture
        .database
        .persist(None, &target, &capture, &page)
        .await?;
    Ok(())
}

async fn transfer_target(fixture: &Fixture) -> Result<()> {
    let page = ParsedPage {
        athletes: vec![Athlete {
            id: "14474869".into(),
            name: "Audrey Koehler".into(),
            school_id: Some("transfer-mn".into()),
            state: Some("MN".into()),
            gender: Some("F".into()),
            grade: Some("12".into()),
            graduation_year: Some(2027),
            profile_url: Some(PROFILE_URL.into()),
            source_values: Some(
                json!({"current_affiliation":{"school_id":"transfer-mn","state":"MN"}}),
            ),
            ..Athlete::default()
        }],
        schools: vec![School {
            id: "transfer-mn".into(),
            name: "Minnesota Transfer School".into(),
            state: Some("MN".into()),
            ..School::default()
        }],
        ..ParsedPage::default()
    };
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        PROFILE_URL.into(),
        PageContext {
            athlete_id: Some("14474869".into()),
            state: Some("MN".into()),
            ..PageContext::default()
        },
    );
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".into(),
        body: serde_json::to_vec(&page)?,
    };
    fixture
        .database
        .persist(None, &target, &capture, &page)
        .await?;
    Ok(())
}

async fn observation(fixture: &Fixture) -> Result<(Option<i64>, Option<i64>, Value)> {
    Ok(
        sqlx::query_as("SELECT athlete_source_id,athlete_id,raw FROM results WHERE id=$1")
            .bind(fixture.result_row_id)
            .fetch_one(fixture.database.pool())
            .await?,
    )
}

#[tokio::test]
async fn given_reviewed_target_transfers_states_when_exporting_acquisition_scope_then_invalid_link_stays_visible()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture = fixture(test).await?;
            retain_acquisition_scope(&fixture).await?;
            let (workspace, _, spec) = manifest(&fixture, test)?;
            assert_eq!(apply(&fixture, &workspace, &spec).await?["links_added"], 1);
            let before = observation(&fixture).await?;
            assert_eq!((before.0, before.1), (None, None));
            transfer_target(&fixture).await?;
            let reason: String = sqlx::query_scalar(
                "SELECT invalidation_reason FROM invalid_reviewed_result_links WHERE source_result_key=$1",
            )
            .bind(&fixture.result_key)
            .fetch_one(fixture.database.pool())
            .await?;
            assert_eq!(reason, "target_snapshot_changed");
            assert_eq!(observation(&fixture).await?, before);
            let (moved, current_issues) = fixture.database.export_snapshot(&["MN".into()], &[2027]).await?;
            assert_eq!(moved.len(), 1);
            assert_eq!(moved.first().context("transferred athlete")?.id, fixture.athlete_id);
            assert!(moved.first().context("transferred athlete")?.performances.is_empty());
            assert_eq!(current_issues.len(), 1);
            let expected = current_issues.first().context("current target issue")?;
            assert_eq!(expected.url, RESULT_URL);
            assert_eq!(expected.detail, "reviewed result link invalidated: target_snapshot_changed | Girls 5000 Meter | 23:03.4 | recorded");
            let (records, issues) = fixture.database.export_snapshot(&["WI".into()], &[2027]).await?;
            assert!(records.is_empty());
            assert_eq!(issues.len(), 1, "source-scoped invalid reviewed result disappeared after target transfer");
            let issue = issues.first().context("original acquisition issue")?;
            assert_eq!((&issue.source, &issue.url, &issue.detail), (&expected.source, &expected.url, &expected.detail));
            let (_, combined) = fixture.database.export_snapshot(&["WI".into(), "MN".into()], &[2027]).await?;
            assert_eq!(combined.len(), 1);
            let (_, excluded) = fixture.database.export_snapshot(&["MI".into()], &[2027]).await?;
            assert!(excluded.is_empty());
            fixture.database.close().await;
            Ok(())
        })
    })
    .await
}
