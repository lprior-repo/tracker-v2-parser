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
    for container in ["row", "table", "history"] {
        masked_export(container).await?;
    }
    Ok(())
}

async fn masked_export(container: &str) -> Result<()> {
    use calamine::{Reader, open_workbook_auto};
    let container = container.to_owned();
    support::with_schema(move |test| {
        Box::pin(async move {
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                "https://www.milesplit.com/athletes/11357806".to_owned(),
                PageContext::default(),
            );
            let body = profile(
                &target,
                "<span class='city-state'>Madison, WI</span>",
                &masked_records(&container),
            )
            .replace("Class of 2026", "Class of 2027");
            let input = test.directory().join("masked.html");
            std::fs::write(&input, &body)?;
            let args = [
                "scrape",
                "--input",
                input.to_str().context("input path")?,
                "--source",
                "milesplit",
                "--kind",
                "athlete",
                "--url",
                &target.url,
            ];
            support::assert_success(&test.cli(&args).await?);
            support::assert_success(&test.cli(&args).await?);
            let database = Database::connect(&test.url).await?;
            let (records, _) = database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            assert_eq!(records.len(), 1);
            assert!(records[0].performances.is_empty(), "{container}");
            assert!(
                records[0].profile_details[0]
                    .source_values
                    .to_string()
                    .contains("10.01")
            );
            let path = test.directory().join("masked.xlsx");
            support::assert_success(
                &test
                    .cli(&["export", "--output", path.to_str().context("output path")?])
                    .await?,
            );
            let mut workbook = open_workbook_auto(&path)?;
            let athletes = workbook.worksheet_range("Athletes")?;
            assert_eq!(athletes.height(), 2);
            assert!(
                !athletes
                    .rows()
                    .flatten()
                    .any(|cell| cell.to_string() == "10.01")
            );
            let details = workbook.worksheet_range("Profile_details")?;
            let json = details
                .rows()
                .skip(1)
                .map(|row| row[6].to_string())
                .collect::<String>();
            let reopened: Value = serde_json::from_str(&json)?;
            assert_eq!(reopened, records[0].profile_details[0].source_values);
            let row_kind = if container == "history" {
                "result_rows"
            } else {
                "pr_rows"
            };
            let rows = reopened["pages"][&target.url][row_kind]
                .as_array()
                .context("reopened source rows")?;
            let matching = rows
                .iter()
                .filter(|row| {
                    row["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("10.01"))
                })
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "{container}");
            assert_eq!(matching[0]["masked"], json!(true), "{container}");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[test]
fn given_hidden_sport_tab_and_locked_ranking_cell_when_parsed_then_public_mark_is_retained()
-> Result<()> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://www.milesplit.com/athletes/11357806".to_owned(),
        PageContext::default(),
    );
    let records = r#"<table id="prTable" style="display:none"><tr data-pr-mode="outdoor" data-pr-level="hs"><td>100m</td><td>10.01</td><td class="mask">99</td><td></td><td></td></tr></table>"#;
    let page = sources::parse(&target, &profile(&target, "", records))?;
    assert_eq!(page.performances.len(), 1);
    assert_eq!(page.performances[0].mark, "10.01");
    Ok(())
}
#[test]
fn given_relative_pr_meet_link_when_profile_parsed_then_result_keeps_published_meet_identity()
-> Result<()> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://www.milesplit.com/athletes/11357806".to_owned(),
        PageContext::default(),
    );
    let records = r#"<table id="prTable"><tr data-pr-mode="outdoor" data-pr-level="hs"><td>100m</td><td>10.01</td><td></td><td><a href="/meets/123-invitational">Invitational</a></td><td>2026-05-01</td></tr></table>"#;
    let page = sources::parse(&target, &profile(&target, "", records))?;
    assert_eq!(page.performances.len(), 1);
    assert_eq!(page.performances[0].meet_id.as_deref(), Some("123"));
    assert_eq!(
        page.performances[0].meet_name.as_deref(),
        Some("Invitational")
    );
    Ok(())
}

#[test]
fn given_legacy_queued_context_when_profile_parsed_then_historical_school_and_state_are_not_current()
-> Result<()> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        "https://www.milesplit.com/athletes/11357806".to_owned(),
        PageContext {
            school_id: Some("4912".to_owned()),
            school_name: Some("Historical School".to_owned()),
            state: Some("AL".to_owned()),
            ..PageContext::default()
        },
    );
    for (school, expected) in [(String::new(), None), (current_school("999"), Some("999"))] {
        let page = sources::parse(&target, &profile(&target, &school, ""))?;
        assert_eq!(page.athletes[0].school_id.as_deref(), expected);
        assert_eq!(page.athletes[0].state, None);
        assert!(page.schools.iter().all(|school| school.state.is_none()
            && school.city.is_none()
            && school.address.is_none()));
    }
    Ok(())
}

fn assert_affiliation_workbook(
    path: &std::path::Path,
    records: &[track_scrapper_v2::model::AthleteRecord],
    expected: &str,
) -> Result<()> {
    use calamine::{Reader, open_workbook_auto};
    use track_scrapper_v2::export::{ExportOptions, write_spreadsheets};
    write_spreadsheets(
        path,
        records,
        &ExportOptions {
            states: vec!["AL".to_owned(), "OH".to_owned(), "WI".to_owned()],
            graduation_years: vec![2026],
            rows_per_file: 100,
            per_athlete_sheets: false,
        },
    )?;
    let mut workbook = open_workbook_auto(path)?;
    let athletes = workbook.worksheet_range("Athletes")?;
    assert_eq!(athletes.height(), 2);
    let header = athletes.rows().next().context("header")?;
    let row = athletes.rows().nth(1).context("athlete")?;
    let school = header
        .iter()
        .position(|cell| cell.to_string() == "school_names")
        .context("school column")?;
    assert_eq!(row[school].to_string(), expected);
    assert!(
        !row.iter()
            .any(|cell| cell.to_string().contains("Historical coach")
                || cell.to_string().contains("historical@school.test")
                || cell.to_string().contains("99 Historical Street"))
    );
    Ok(())
}

#[tokio::test]
async fn given_pre_fix_contaminated_affiliation_when_profile_replayed_then_workbook_clears_historical_contacts_and_address()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let (target, body) = meet();
            let mut legacy = sources::parse(&target, &body)?;
            let historical = legacy.performances[0]
                .school_id
                .clone()
                .context("historical school")?;
            legacy.athletes[0].school_id = Some(historical.clone());
            legacy.athletes[0].state = Some("AL".to_owned());
            legacy.schools[0].address = Some("99 Historical Street".to_owned());
            legacy.schools[0].state = Some("AL".to_owned());
            legacy.coaches.push(track_scrapper_v2::model::Coach {
                school_id: historical.clone(),
                name: Some("Historical coach".to_owned()),
                role: "Head Coach".to_owned(),
                email: Some("historical@school.test".to_owned()),
                sport: Some(Sport::Outdoor),
                review_notes: vec![],
            });
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".to_owned(),
                body: body.as_bytes().to_vec(),
            };
            database.persist(None, &target, &capture, &legacy).await?;
            let (before, _) = database
                .export_snapshot(
                    &["AL".to_owned(), "OH".to_owned(), "WI".to_owned()],
                    &[2026],
                )
                .await?;
            assert_eq!(before[0].schools[0].id, historical);
            assert_eq!(before[0].coaches.len(), 1);
            let mut next = legacy.next[0].clone();
            next.context.school_id = Some(historical.clone());
            next.context.state = Some("AL".to_owned());
            for (school, expected) in [
                (String::new(), ""),
                (current_school("999"), "Current School 999"),
            ] {
                persist(&database, &next, &profile(&next, &school, "")).await?;
                persist(&database, &next, &profile(&next, &school, "")).await?;
                let (records, _) = database
                    .export_snapshot(
                        &["AL".to_owned(), "OH".to_owned(), "WI".to_owned()],
                        &[2026],
                    )
                    .await?;
                assert!(records[0].coaches.is_empty());
                assert!(records[0].schools.iter().all(|school| school.id == "999"
                    && school.address.is_none()
                    && school.state.is_none()));
                assert_affiliation_workbook(
                    &test
                        .directory()
                        .join(format!("affiliation-{}.xlsx", records[0].schools.len())),
                    &records,
                    expected,
                )?;
                let state: Option<String> = sqlx::query_scalar("SELECT state FROM athlete_sources")
                    .fetch_one(&test.pool)
                    .await?;
                assert_eq!(state, None);
            }
            let represented: String = sqlx::query_scalar(
                "SELECT s.source_id FROM results r JOIN schools s ON s.id=r.represented_school_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(represented, historical);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_authoritative_empty_profile_when_roster_arrives_then_context_cannot_backfill_affiliation()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let profile_target = Target::get(Source::MileSplit, PageKind::Athlete, "https://www.milesplit.com/athletes/11357806".to_owned(), PageContext::default());
        persist(&database, &profile_target, &profile(&profile_target, "", "")).await?;
        let roster_target = Target::get(Source::MileSplit, PageKind::Roster, "https://al.milesplit.com/teams/4912-old/roster".to_owned(), PageContext { state: Some("AL".to_owned()), season: Some(2026), ..PageContext::default() });
        let roster = format!(r#"<html><head><link rel="canonical" href="{}"></head><body><div class="jumbotron-content"><h1>Old School</h1></div><ul id="rosterDataset"><li class="athlete-row"><a href="/athletes/11357806">Ousley, Payton</a><span class="column-grad-year">2026</span><span class="column-gender">F</span><span data-season-id="2"><svg class="icon-yes"></svg></span></li></ul></body></html>"#, roster_target.url);
        persist(&database, &roster_target, &roster).await?;
        let observed: (Option<i64>, Option<String>) = sqlx::query_as("SELECT school_id,state FROM athlete_sources").fetch_one(&test.pool).await?;
        assert_eq!(observed, (None, None));
        let (records, _) = database.export_snapshot(&["AL".to_owned(), "OH".to_owned(), "WI".to_owned()], &[2026]).await?;
        assert_eq!(records.len(), 1);
        assert!(records[0].schools.is_empty());
        assert_affiliation_workbook(&test.directory().join("profile-then-roster.xlsx"), &records, "")?;
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_legacy_context_state_when_current_profile_omits_location_then_projection_is_uncertain_without_erasing_school_fact()
-> Result<()> {
    for (stored_state, expected) in [("AL", None), ("OH", Some("OH"))] {
        legacy_school_state(stored_state, expected).await?;
    }
    Ok(())
}

async fn legacy_school_state(stored_state: &str, expected: Option<&str>) -> Result<()> {
    let stored_state = stored_state.to_owned();
    let expected = expected.map(str::to_owned);
    support::with_schema(move |test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = Target::get(Source::MileSplit, PageKind::Athlete, "https://www.milesplit.com/athletes/11357806".to_owned(), PageContext::default());
        let body = profile(&target, &current_school("999"), "");
        let mut legacy = sources::parse(&target, &body)?;
        legacy.athletes[0].state = Some("AL".to_owned());
        legacy.athletes[0].source_values.as_mut().and_then(Value::as_object_mut).context("legacy values")?.remove("current_affiliation");
        legacy.schools[0].state = Some(stored_state.clone());
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "text/html".to_owned(), body: body.as_bytes().to_vec() };
        database.persist(None, &target, &capture, &legacy).await?;
        for _ in 0..2 {
            persist(&database, &target, &body).await?;
        }
        persist(&database, &target, &profile(&target, "", "")).await?;
        let (absent, _) = database.export_snapshot(&["AL".to_owned(), "OH".to_owned(), "WI".to_owned()], &[2026]).await?;
        assert!(absent[0].schools.is_empty());
        persist(&database, &target, &body).await?;
        let global: String = sqlx::query_scalar("SELECT state FROM schools WHERE source_id='999'").fetch_one(&test.pool).await?;
        assert_eq!(global, stored_state);
        let (records, _) = database.export_snapshot(&["AL".to_owned(), "OH".to_owned(), "WI".to_owned()], &[2026]).await?;
        assert_eq!(records[0].schools[0].state, expected);
        let marker = &records[0].profile_details[0].source_values["current_affiliation"];
        assert_eq!(marker["school_state_unverified"], json!(expected.is_none()));
        if expected.is_none() {
            assert!(records[0].needs_review);
            let (other_state, _) = database.export_snapshot(&["WI".to_owned()], &[2026]).await?;
            assert_eq!(other_state.len(), 1, "unverified legacy state must not filter out the athlete");
        }
        assert_affiliation_workbook(&test.directory().join("legacy-state.xlsx"), &records, "Current School 999")?;
        let located = profile(&target, &format!("{}<span class='city-state'>Columbus, OH</span>", current_school("999")), "");
        persist(&database, &target, &located).await?;
        let (confirmed, _) = database.export_snapshot(&["OH".to_owned()], &[2026]).await?;
        assert_eq!(confirmed[0].schools[0].state.as_deref(), Some("OH"));
        assert_eq!(confirmed[0].profile_details[0].source_values["current_affiliation"]["school_state_unverified"], json!(false));
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_original_historical_a_then_legacy_profile_b_when_replayed_then_b_state_is_not_trusted()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let (target, body) = meet();
        let mut historical = sources::parse(&target, &body)?;
        let school_a = historical.performances[0].school_id.clone().context("historical A")?;
        historical.athletes[0].school_id = Some(school_a.clone());
        historical.athletes[0].state = Some("AL".to_owned());
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".to_owned(), body: body.as_bytes().to_vec() };
        database.persist(None, &target, &capture, &historical).await?;
        let next = &historical.next[0];
        let profile_body = profile(next, &current_school("999"), "");
        let mut legacy_b = sources::parse(next, &profile_body)?;
        legacy_b.athletes[0].source_values.as_mut().and_then(Value::as_object_mut).context("legacy profile evidence")?.remove("current_affiliation");
        legacy_b.athletes[0].state = Some("AL".to_owned());
        legacy_b.schools[0].state = Some("AL".to_owned());
        let capture = Capture { url: next.url.clone(), status: 200, content_type: "text/html".to_owned(), body: profile_body.as_bytes().to_vec() };
        database.persist(None, next, &capture, &legacy_b).await?;
        let sticky: (String, String) = sqlx::query_as("SELECT s.source_id,p.details->>'school_id' FROM athlete_sources p JOIN schools s ON s.id=p.school_id").fetch_one(&test.pool).await?;
        assert_eq!(sticky, (school_a, "999".to_owned()));
        persist(&database, next, &profile_body).await?;
        let (records, _) = database.export_snapshot(&["WI".to_owned()], &[2026]).await?;
        assert_eq!(records.len(), 1, "uncertain B state must not exclude the athlete");
        assert_eq!(records[0].schools[0].id, "999");
        assert_eq!(records[0].schools[0].state, None);
        assert_eq!(records[0].profile_details[0].source_values["current_affiliation"]["school_state_unverified"], json!(true));
        let stored: String = sqlx::query_scalar("SELECT state FROM schools WHERE source_id='999'").fetch_one(&test.pool).await?;
        assert_eq!(stored, "AL", "global school fact is retained");
        assert_affiliation_workbook(&test.directory().join("sticky-a-to-b.xlsx"), &records, "Current School 999")?;
        database.close().await;
        Ok(())
    })).await
}
