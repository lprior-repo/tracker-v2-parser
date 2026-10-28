use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use track_scrapper_v2::model::{Sport, Target};
use track_scrapper_v2::sources::athleticnet::{athlete_target, meet_target, parse};

const MEET: &str = include_str!("../fixtures/athleticnet/meet-634313.json");
const RESULTS: &str = include_str!("../fixtures/athleticnet/results-634313.json");
const BIO: &str = include_str!("../fixtures/athleticnet/bio-28872883.json");

fn results_target() -> Result<Target> {
    let metadata = parse(&meet_target("634313", Sport::Outdoor)?, MEET)?;
    metadata.next.into_iter().next().context("results target")
}

fn result_row(id: i64, name: &str, date: Option<&str>) -> Value {
    json!({"IDResult":id,"AthleteID":id,"FirstName":name,"LastName":"Runner",
        "Result":"12.34a","ResultDate":date,"TeamID":3204})
}

#[test]
fn given_scoped_meet_when_new_athletes_are_followed_then_scope_does_not_become_location()
-> Result<()> {
    let mut target = results_target()?;
    target.context.state = Some("WI".into());
    let page = parse(&target, RESULTS)?;
    assert_eq!(page.next.len(), 516);
    assert!(page.athletes.iter().all(|athlete| athlete.state.is_none()));
    assert!(page.next.iter().all(|next| next.context.state.is_none()
        && next.context.acquisition_state.as_deref() == Some("WI")));
    let followup = page
        .next
        .iter()
        .find(|next| next.context.athlete_id.as_deref() == Some("28872883"))
        .context("Kingston bio followup")?;
    let bio = parse(followup, BIO)?;
    assert!(bio.athletes.iter().all(|athlete| athlete.state.is_none()));
    assert!(bio.schools.iter().all(|school| school.state.is_none()));
    assert!(
        bio.next
            .iter()
            .all(|next| next.context.acquisition_state.as_deref() == Some("WI"))
    );
    Ok(())
}

#[test]
fn given_published_event_types_when_meet_parsed_then_specs_and_categories_stay_distinct()
-> Result<()> {
    let body: Value = serde_json::from_str(RESULTS)?;
    let page = parse(&results_target()?, RESULTS)?;
    let expected = [
        (10, 0, "39\" / 0.991m"),
        (11, 0, "36\" / 0.914m"),
        (12, 0, "12lb"),
        (12, 99, "12lb Seated"),
        (13, 0, "1.6kg"),
        (28, 0, "33\" / 0.838m"),
        (29, 0, "30\" / 0.762m"),
        (30, 0, "4kg"),
        (30, 99, "4kg Seated"),
        (31, 0, "1kg"),
        (1, 99, "Wheelchair"),
        (3, 99, "Wheelchair"),
        (4, 99, "Wheelchair"),
        (19, 99, "Wheelchair"),
        (21, 99, "Wheelchair"),
        (22, 99, "Wheelchair"),
        (53, 99, "Wheelchair"),
    ];
    let mut specifications = 0;
    let mut wheelchairs = 0;
    for block in body["flatEvents"].as_array().context("blocks")? {
        for row in block["results"].as_array().context("rows")? {
            let description = expected
                .iter()
                .find(|(event, kind, _)| row["EventID"] == *event && row["EventTypeID"] == *kind);
            let result = page.performances.iter().find(|result| {
                result.result_id == row["IDResult"].as_i64().map(|id| id.to_string())
            });
            let Some(result) = result else { continue };
            assert_eq!(result.mark, row["Result"].as_str().context("mark")?);
            if let Some((_, _, description)) = description {
                assert_eq!(
                    result.event,
                    format!(
                        "{} ({description})",
                        block["Event"].as_str().context("event")?
                    )
                );
                assert_eq!(
                    result
                        .source_values
                        .as_ref()
                        .context("classification evidence")?["eventType"]["Description"],
                    *description
                );
                if *description == "Wheelchair" {
                    wheelchairs += 1;
                } else {
                    specifications += 1;
                }
            } else {
                assert_eq!(result.event, block["Event"].as_str().context("event")?);
            }
        }
    }
    assert_eq!((specifications, wheelchairs), (185, 12));
    assert_eq!(page.performances.len(), 686);
    assert_eq!(page.unassigned_results.len(), 72);
    Ok(())
}

#[test]
fn given_mixed_row_types_when_parsed_then_each_row_uses_its_own_published_classification()
-> Result<()> {
    let body = json!({"teams":[],"eventTypes":[
        {"eventId":1,"IDEventType":99,"Description":"Wheelchair"},
        {"eventId":12,"IDEventType":99,"Description":"12lb Seated"}],
        "flatEvents":[{"Event":"100 Meters","EventId":1,"IDEventType":0,"results":[
            {"IDResult":1,"AthleteID":1,"FirstName":"Public","Result":"15.00a","EventID":1,"EventTypeID":99},
            {"IDResult":2,"AthleteID":2,"FirstName":"Public","Result":"12.00a","EventID":1,"EventTypeID":0}]},
            {"Event":"Shot Put","EventId":12,"IDEventType":0,"results":[
            {"IDResult":3,"Result":"20-00","EventID":12,"EventTypeID":99}]}]});
    let page = parse(&results_target()?, &body.to_string())?;
    assert_eq!(page.performances[0].event, "100 Meters (Wheelchair)");
    assert_eq!(page.performances[1].event, "100 Meters");
    assert_eq!(page.unassigned_results[0].event, "Shot Put (12lb Seated)");
    assert_eq!(
        page.unassigned_results[0].raw["eventType"]["Description"],
        "12lb Seated"
    );
    Ok(())
}

#[test]
fn given_row_dates_when_context_is_present_or_missing_then_published_row_date_wins() -> Result<()> {
    let body = json!({"teams":[],"flatEvents":[{"Event":"100 Meters","results":[
        result_row(1,"Public",Some("2026-05-16T12:00:00")),
        result_row(2,"Public",None), result_row(3,"Public",Some("invalid")),
        result_row(4,"••••",Some("2026-05-17"))]},
        {"Event":"4x100 Relay","isPersonal":false,"results":[result_row(5,"Team",Some("2026-05-18"))]}]});
    for fallback in [Some("2026-05-15"), None] {
        let mut target = results_target()?;
        target.context.meet_date = fallback.map(str::to_owned);
        let page = parse(&target, &body.to_string())?;
        assert_eq!(page.performances[0].date.as_deref(), Some("2026-05-16"));
        assert_eq!(page.performances[1].date.as_deref(), fallback);
        assert_eq!(page.performances[2].date.as_deref(), fallback);
        assert_eq!(
            page.unassigned_results[0].date.as_deref(),
            Some("2026-05-17")
        );
        assert_eq!(
            page.unassigned_results[1].date.as_deref(),
            Some("2026-05-18")
        );
    }
    Ok(())
}

#[test]
fn given_masked_names_when_meet_or_bio_parsed_then_no_person_is_manufactured() -> Result<()> {
    for mask in ["••••", "****"] {
        let body = json!({"teams":[],"flatEvents":[{"Event":"100 Meters","results":[result_row(1,mask,None)]}]});
        let page = parse(&results_target()?, &body.to_string())?;
        assert!(page.athletes.is_empty());
        assert!(page.performances.is_empty());
        assert!(page.next.is_empty());
        assert_eq!(page.unassigned_results[0].mark, "12.34a");
        let bio = json!({"athlete":{"IDAthlete":1,"FirstName":mask},"resultsTF":[{"IDResult":1,"Result":"12.34a","EventID":1}],"eventsTF":[{"IDEvent":1,"Event":"100 Meters"}]});
        let page = parse(
            &athlete_target("1", None, Sport::Outdoor)?,
            &bio.to_string(),
        )?;
        assert!(page.athletes.is_empty());
        assert!(page.performances.is_empty());
        assert_eq!(page.unassigned_results[0].mark, "12.34a");
    }
    Ok(())
}

fn fixture_hygiene(value: &Value) -> Result<()> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                assert!(
                    ![
                        "ownerid",
                        "contactuserid",
                        "setupstate",
                        "agreestart",
                        "activate",
                        "jwt",
                        "token",
                        "cookie",
                        "authorization"
                    ]
                    .iter()
                    .any(|forbidden| key.contains(forbidden))
                        && key != "ip"
                );
                fixture_hygiene(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                fixture_hygiene(value)?;
            }
        }
        Value::String(text) => {
            if text.trim_start().starts_with(['{', '[']) {
                fixture_hygiene(&serde_json::from_str::<Value>(text)?)?;
            }
            assert!(
                !text
                    .split(|c: char| !c.is_ascii_digit() && c != '.')
                    .any(|candidate| candidate.split('.').count() == 4
                        && candidate.parse::<std::net::Ipv4Addr>().is_ok())
            );
        }
        _ => {}
    }
    Ok(())
}

#[test]
fn given_sanitized_metadata_when_outer_and_embedded_json_inspected_then_athletic_evidence_remains()
-> Result<()> {
    let value: Value = serde_json::from_str(MEET)?;
    fixture_hygiene(&value)?;
    assert_eq!(value["meet"]["Name"], "Big 8 Conference");
    assert_eq!(value["meet"]["SeasonID"], 2026);
    assert!(value["meet"]["Location"].is_object());
    assert!(value["meet"]["TeamScores"].is_array());
    let metadata = parse(&meet_target("634313", Sport::Outdoor)?, MEET)?;
    assert_eq!(
        metadata.next[0].context.meet_date.as_deref(),
        Some("2026-05-15")
    );
    assert_eq!(parse(&metadata.next[0], RESULTS)?.performances.len(), 686);
    Ok(())
}

#[test]
fn given_generic_track_seasons_when_rows_have_indoor_override_then_request_context_does_not_guess()
-> Result<()> {
    let body = json!({"athlete":{"IDAthlete":1,"FirstName":"Public","LastName":"Runner","SchoolID":3204},
        "allTeams":{"3204":{"SchoolName":"Middleton"}},
        "allSeasons":[{"SchoolID":3204,"IDSeason":2026,"Display":"TrackAndField"}],
        "eventsTF":[{"IDEvent":1,"Event":"60 Meters","PersonalEvent":true}],
        "resultsTF":[{"IDResult":1,"EventID":1,"SchoolID":3204,"SeasonID":2026,"Result":"7.90a","overrideIndoor":true},
        {"IDResult":2,"EventID":1,"SchoolID":3204,"SeasonID":2026,"Result":"7.80a","overrideIndoor":false},
        {"IDResult":3,"EventID":1,"SchoolID":3204,"SeasonID":2026,"Result":"7.70a"}],"resultsXC":[]});
    for sport in [Sport::Outdoor, Sport::Indoor] {
        let page = parse(&athlete_target("1", None, sport)?, &body.to_string())?;
        assert!(page.performances.is_empty());
        assert_eq!(page.unassigned_results.len(), 3);
        assert_eq!(page.unassigned_results[0].raw["overrideIndoor"], true);
        assert_eq!(page.unassigned_results[1].raw["overrideIndoor"], false);
        assert_eq!(page.unassigned_results[2].raw["IDResult"], 3);
        assert!(
            page.notes
                .iter()
                .any(|note| note.contains("season evidence"))
        );
    }
    Ok(())
}

#[test]
fn given_bio_event_variants_when_joined_then_same_event_id_does_not_mix_categories() -> Result<()> {
    let body = json!({"athlete":{"IDAthlete":1,"FirstName":"Public","SchoolID":3204},
        "allTeams":{"3204":{"SchoolName":"Middleton"}},
        "allSeasons":[{"SchoolID":3204,"IDSeason":2026,"Display":"2026 Outdoor"}],
        "eventsTF":[{"IDEvent":12,"IDEventType":0,"Event":"Shot Put","Description":"12lb"},
            {"IDEvent":12,"IDEventType":99,"Event":"Shot Put","Description":"12lb Seated"}],
        "resultsTF":[{"IDResult":1,"EventID":12,"EventTypeID":0,"SchoolID":3204,"SeasonID":2026,"Result":"40-00"},
            {"IDResult":2,"EventID":12,"EventTypeID":99,"SchoolID":3204,"SeasonID":2026,"Result":"20-00"}],
        "resultsXC":[]});
    let page = parse(
        &athlete_target("1", None, Sport::Outdoor)?,
        &body.to_string(),
    )?;
    assert_eq!(page.performances.len(), 2);
    assert_eq!(page.performances[0].event, "Shot Put (12lb)");
    assert_eq!(page.performances[1].event, "Shot Put (12lb Seated)");
    assert_eq!(
        page.performances[1]
            .source_values
            .as_ref()
            .context("type evidence")?["eventType"]["IDEventType"],
        99
    );
    Ok(())
}

#[test]
fn given_direct_bio_scope_when_school_state_is_missing_then_location_stays_unknown() -> Result<()> {
    for legacy in [false, true] {
        let mut target = athlete_target("28872883", None, Sport::Outdoor)?;
        if legacy {
            target.context.state = Some("WI".to_owned());
        } else {
            target.context.acquisition_state = Some("WI".to_owned());
        }
        let page = parse(&target, BIO)?;
        assert!(page.athletes.iter().all(|athlete| athlete.state.is_none()));
        assert!(page.schools.iter().all(|school| school.state.is_none()));
        assert!(
            page.next
                .iter()
                .all(|next| next.context.acquisition_state.as_deref() == Some("WI"))
        );
        assert!(
            page.notes
                .iter()
                .any(|note| note.contains("School 3204 has no published state"))
        );
    }
    Ok(())
}

#[test]
fn given_published_school_state_when_scope_differs_then_published_fact_is_retained() -> Result<()> {
    let mut target = athlete_target("28872883", None, Sport::Outdoor)?;
    target.context.acquisition_state = Some("WI".to_owned());
    let mut body: Value = serde_json::from_str(BIO)?;
    body["allTeams"]["3204"]["State"] = json!("IL");
    let page = parse(&target, &body.to_string())?;
    assert_eq!(page.schools[0].state.as_deref(), Some("IL"));
    assert!(page.athletes[0].state.is_none());
    assert!(
        page.next
            .iter()
            .all(|next| next.context.acquisition_state.as_deref() == Some("WI"))
    );
    Ok(())
}
