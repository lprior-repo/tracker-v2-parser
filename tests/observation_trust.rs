mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

fn meet() -> (Target, String) {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Results,
        "https://al.milesplit.com/api/v1/meets/725218/performances?isMeetPro=0".to_owned(),
        PageContext {
            subject_id: Some("725218".to_owned()),
            result_set_ids: vec!["1266814".to_owned()],
            sport: Some(Sport::Outdoor),
            ..PageContext::default()
        },
    );
    let original: Value = serde_json::from_str(include_str!(
        "../fixtures/milesplit/performances-al-725218.json"
    ))
    .unwrap();
    let row = original["data"][0].clone();
    (
        target,
        json!({"_meta":{"status_code":200},"data":[row]}).to_string(),
    )
}

fn profile(target: &Target, school: &str, records: &str) -> String {
    format!(
        r#"<html><head><link rel="canonical" href="{}"></head><body><h1 id="athleteName">Payton Ousley</h1><div class="athlete-info">{school}<span class="grad-year">Class of 2026</span></div><nav class="profile-navigation"></nav>{records}</body></html>"#,
        target.url
    )
}

fn current_school(id: &str) -> String {
    format!(r#"<span class="current-school"><a href="/teams/{id}">Current School {id}</a></span>"#)
}

#[test]
fn given_historical_meet_when_following_generated_profile_then_only_published_current_school_is_used()
-> Result<()> {
    let (target, body) = meet();
    let page = sources::parse(&target, &body)?;
    let historical = page.performances[0]
        .school_id
        .clone()
        .context("represented school")?;
    let next = &page.next[0];
    assert_eq!(page.athletes[0].school_id, None);
    assert_eq!(next.context.school_id, None);
    let absent = sources::parse(next, &profile(next, "", ""))?;
    assert_eq!(absent.athletes[0].school_id, None);
    assert_eq!(absent.athletes[0].state, None);
    let explicit = sources::parse(next, &profile(next, &current_school("999"), ""))?;
    assert_eq!(explicit.athletes[0].school_id.as_deref(), Some("999"));
    assert_ne!(historical, "999");
    Ok(())
}

#[test]
fn given_raw_historical_meet_when_following_generated_profile_then_school_is_only_result_evidence()
-> Result<()> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Results,
        "https://oh.milesplit.com/meets/770621/results/1321880/raw".to_owned(),
        PageContext {
            sport: Some(Sport::CrossCountry),
            ..PageContext::default()
        },
    );
    let body = include_str!("../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html")
        .replacen("Jeydyn Fields", "<a href='/athletes/123456'>Jeydyn Fields</a>", 1)
        .replacen("8 Jackson                                    12:40.6", "8 <a href='/teams/98765-jackson'>Jackson</a>                                    12:40.6", 1);
    let page = sources::parse(&target, &body)?;
    assert_eq!(page.performances[0].school_id.as_deref(), Some("98765"));
    assert_eq!(page.athletes[0].school_id, None);
    let next = page
        .next
        .iter()
        .find(|next| next.kind == PageKind::Athlete)
        .context("profile target")?;
    assert_eq!(next.context.school_id, None);
    for (school, expected) in [(String::new(), None), (current_school("999"), Some("999"))] {
        let parsed = sources::parse(next, &profile(next, &school, ""))?;
        assert_eq!(parsed.athletes[0].school_id.as_deref(), expected);
    }
    Ok(())
}

fn masked_records(container: &str) -> String {
    let row = r#"<tr data-pr-mode="outdoor" data-pr-level="hs"><td>100m</td><td>10.01</td><td></td><td></td><td></td></tr>"#;
    match container {
        "row" => format!("<table id='prTable'>{}</table>", row.replace("<tr ", "<tr class='mask' ")),
        "table" => format!("<table id='prTable' class='paywall'>{row}</table>"),
        _ => "<div id='stats' class='record--locked'><div class='record' data-season='outdoor' data-event='100m' data-level='hs'><div class='seed'>10.01</div></div></div>".to_owned(),
    }
}

#[test]
fn given_masked_ancestors_when_public_profile_parsed_then_numeric_marks_remain_only_evidence()
-> Result<()> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://www.milesplit.com/athletes/11357806".to_owned(),
        PageContext::default(),
    );
    for container in ["row", "table", "history"] {
        let page = sources::parse(&target, &profile(&target, "", &masked_records(container)))?;
        assert!(page.performances.is_empty(), "{container}");
        assert!(
            page.athletes[0]
                .source_values
                .as_ref()
                .context("evidence")?
                .to_string()
                .contains("10.01")
        );
    }
    Ok(())
}

async fn persist(database: &Database, target: &Target, body: &str) -> Result<()> {
    let page = sources::parse(target, body)?;
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".to_owned(),
        body: body.as_bytes().to_vec(),
    };
    database.persist(None, target, &capture, &page).await?;
    Ok(())
}

#[tokio::test]
async fn given_historical_meet_when_current_school_changes_then_persisted_affiliation_follows_profile()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let (target, body) = meet();
            let page = sources::parse(&target, &body)?;
            let next = &page.next[0];
            persist(&database, &target, &body).await?;
            persist(&database, next, &profile(next, "", "")).await?;
            let unknown: Option<i64> = sqlx::query_scalar("SELECT school_id FROM athlete_sources")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(unknown, None);
            for id in ["998", "999", "999"] {
                persist(&database, next, &profile(next, &current_school(id), "")).await?;
            }
            persist(&database, &target, &body).await?;
            let current: String = sqlx::query_scalar(
                "SELECT s.source_id FROM athlete_sources a JOIN schools s ON s.id=a.school_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(current, "999");
            let canonical: String = sqlx::query_scalar(
                "SELECT s.source_id FROM athletes a JOIN schools s ON s.id=a.school_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(canonical, "999");
            let represented: String = sqlx::query_scalar(
                "SELECT s.source_id FROM results r JOIN schools s ON s.id=r.represented_school_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(Some(represented), page.performances[0].school_id);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_masked_profile_rows_when_exported_then_workbook_has_no_numeric_pr_and_keeps_evidence()
-> Result<()> {
    use calamine::{Reader, open_workbook_auto};
    use track_scrapper_v2::export::{ExportOptions, write_spreadsheets};
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                "https://www.milesplit.com/athletes/11357806".to_owned(),
                PageContext::default(),
            );
            let records = ["row", "table", "history"].map(masked_records).join("");
            let body = profile(
                &target,
                "<span class='city-state'>Madison, WI</span>",
                &records,
            );
            persist(&database, &target, &body).await?;
            let (records, _) = database
                .export_snapshot(&["WI".to_owned()], &[2026])
                .await?;
            assert_eq!(records.len(), 1);
            assert!(records[0].performances.is_empty());
            assert!(
                records[0].profile_details[0]
                    .source_values
                    .to_string()
                    .contains("10.01")
            );
            let path = test.directory().join("masked.xlsx");
            write_spreadsheets(
                &path,
                &records,
                &ExportOptions {
                    states: vec!["WI".to_owned()],
                    graduation_years: vec![2026],
                    rows_per_file: 100,
                    per_athlete_sheets: false,
                },
            )?;
            let mut workbook = open_workbook_auto(&path)?;
            let athletes = workbook.worksheet_range("Athletes")?;
            assert_eq!(athletes.height(), 2);
            assert!(
                !athletes
                    .rows()
                    .flatten()
                    .any(|cell| cell.to_string() == "10.01")
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
