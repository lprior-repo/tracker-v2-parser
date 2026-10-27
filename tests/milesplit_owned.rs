use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    model::{PageContext, PageKind, Source, Sport, Target},
    sources::milesplit,
};

fn target() -> Target {
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

#[test]
fn given_real_response_with_invalid_year_when_parsed_then_claim_is_reviewed_without_losing_results()
-> Result<()> {
    let mut request = target();
    request.context.result_set_ids.push("1266815".to_owned());
    let captured = include_str!("../fixtures/milesplit/performances-al-725218.json");
    let page = milesplit::parse(&request, captured)?;
    let first = page
        .performances
        .iter()
        .find(|p| p.result_id.as_deref() == Some("201782806"))
        .ok_or_else(|| anyhow::anyhow!("missing captured result"))?;
    assert_eq!(first.athlete_id, "11357806");
    assert_eq!(first.mark, "12.40");
    assert_eq!(first.meet_id.as_deref(), Some("725218"));
    assert!(page.athletes.iter().any(|a| a.id == "11357806"
        && a.profile_url.as_deref()
            == Some("https://www.milesplit.com/athletes/11357806-payton-ousley")));
    assert!(
        page.athletes
            .iter()
            .any(|a| a.id == "17659726" && a.needs_review && a.graduation_year.is_none())
    );
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("17659726") && note.contains("\"9\""))
    );
    assert!(page.performances.iter().any(|p| {
        p.athlete_id == "17659726"
            && p.source_values
                .as_ref()
                .is_some_and(|v| v["gradYear"] == "9")
    }));
    assert!(
        page.athletes
            .iter()
            .any(|a| a.id == "11357806" && a.graduation_year == Some(2026))
    );
    assert!(!page.performances.iter().any(|p| p.is_relay));
    assert!(
        page.notes
            .iter()
            .any(|note| note.contains("completeness is unknown"))
    );
    Ok(())
}

#[test]
fn given_wrong_owner_and_relay_when_parsed_then_rows_remain_unassigned() -> Result<()> {
    let original: Value = serde_json::from_str(include_str!(
        "../fixtures/milesplit/performances-al-725218.json"
    ))?;
    let mut mismatch = original["data"][0].clone();
    mismatch["profileUrl"] = json!("https://www.milesplit.com/athletes/999-wrong");
    let mut relay = original["data"][0].clone();
    relay["id"] = json!("999991");
    relay["eventName"] = json!("4x100 Meter Relay");
    let mut foreign = original["data"][0].clone();
    foreign["id"] = json!("999992");
    foreign["meetId"] = json!("9");
    let body = json!({"_meta":{"status_code":200}, "data":[mismatch, relay, foreign]}).to_string();
    let page = milesplit::parse(&target(), &body)?;
    assert!(page.performances.is_empty());
    assert!(page.athletes.is_empty());
    assert_eq!(page.unassigned_results.len(), 3);
    assert!(page.unassigned_results[1].is_relay);
    assert_eq!(page.unassigned_results[0].raw["athleteId"], "11357806");
    assert_eq!(page.unassigned_results[2].meet_id.as_deref(), Some("9"));
    assert_eq!(page.unassigned_results[2].date, None);
    Ok(())
}

#[test]
fn given_mismatched_embedded_meet_when_parsed_then_capture_is_rejected() {
    let body = json!({"_meta":{"status_code":200}, "_embedded":{"meet":{"id":"9"}}, "data":[]})
        .to_string();
    assert!(milesplit::parse(&target(), &body).is_err());
}

#[test]
fn given_access_denial_in_json_when_parsed_then_it_is_an_access_stop() -> Result<()> {
    let body = json!({"_meta":{"status_code":403}, "data":[]}).to_string();
    let error = milesplit::parse(&target(), &body)
        .err()
        .ok_or_else(|| anyhow::anyhow!("denial was accepted"))?;
    assert!(error.to_string().contains("access denied"));
    Ok(())
}

#[test]
fn given_multiple_published_files_when_one_response_is_parsed_then_both_are_admitted() -> Result<()>
{
    let mut request = target();
    request.context.result_set_ids.push("1266815".to_owned());
    let body = include_str!("../fixtures/milesplit/performances-al-725218.json");
    let one = milesplit::parse(&target(), body)?;
    let both = milesplit::parse(&request, body)?;
    assert!(both.performances.len() > one.performances.len());
    assert!(
        one.unassigned_results
            .iter()
            .any(|row| row.raw["meetResultsId"] == "1266815")
    );
    Ok(())
}

#[test]
fn given_idless_rows_reordered_when_parsed_then_each_row_has_distinct_evidence() -> Result<()> {
    let original: Value = serde_json::from_str(include_str!(
        "../fixtures/milesplit/performances-al-725218.json"
    ))?;
    let mut first = original["data"][0].clone();
    first["id"] = Value::Null;
    let mut second = first.clone();
    second["mark"] = json!("12.41");
    let body = |row: &Value| json!({"_meta":{"status_code":200}, "data":[row]}).to_string();
    let left = milesplit::parse(&target(), &body(&first))?;
    let right = milesplit::parse(&target(), &body(&second))?;
    assert_ne!(
        left.unassigned_results[0].source_row,
        right.unassigned_results[0].source_row
    );
    Ok(())
}

#[test]
fn given_unsupported_graduation_year_when_parsed_then_athlete_is_flagged_and_result_retained()
-> Result<()> {
    let original: Value = serde_json::from_str(include_str!(
        "../fixtures/milesplit/performances-al-725218.json"
    ))?;
    for year in [json!("2201"), json!("not-a-year")] {
        let mut row = original["data"][0].clone();
        row["gradYear"] = year.clone();
        let body = json!({"_meta":{"status_code":200}, "data":[row]}).to_string();
        let page = milesplit::parse(&target(), &body)?;
        assert_eq!(page.athletes[0].graduation_year, None);
        assert!(page.athletes[0].needs_review);
        assert_eq!(
            page.performances[0]
                .source_values
                .as_ref()
                .map(|raw| &raw["gradYear"]),
            Some(&year)
        );
        assert!(page.notes.iter().any(|note| note.contains("11357806")));
    }
    Ok(())
}

#[test]
fn given_dq_with_numeric_mark_when_parsed_then_source_mark_and_status_survive() -> Result<()> {
    let original: Value = serde_json::from_str(include_str!(
        "../fixtures/milesplit/performances-al-725218.json"
    ))?;
    let mut row = original["data"][0].clone();
    row["statusCode"] = json!("DQ");
    let body = json!({"_meta":{"status_code":200}, "data":[row.clone()]}).to_string();
    let page = milesplit::parse(&target(), &body)?;
    let performance = &page.performances[0];
    assert_eq!(performance.mark, "12.40");
    assert_eq!(performance.reported_status.as_deref(), Some("DQ"));
    assert_eq!(performance.source_values.as_ref(), Some(&row));
    assert!(track_scrapper_v2::marks::normalized_performance(performance).is_none());
    assert!(
        track_scrapper_v2::marks::comparable_performance(
            Source::MileSplit,
            &target().url,
            performance
        )
        .is_none()
    );
    Ok(())
}
