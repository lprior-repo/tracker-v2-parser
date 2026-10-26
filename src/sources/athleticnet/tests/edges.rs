use anyhow::Result;
use serde_json::json;

use super::{BIO, MEET, Sport, athlete_target, meet_target, parse, seeds};
#[test]
fn given_xc_and_indoor_rows_when_parsed_then_sports_and_raw_marks_are_not_conflated() -> Result<()>
{
    let body = json!({"athlete":{"IDAthlete":42,"FirstName":"Public","LastName":"Runner","SchoolID":3204,"Gender":"F"},"allTeams":{"3204":{"IDSchool":3204,"SchoolName":"Middleton"}},"allSeasons":[{"SchoolID":3204,"IDSeason":12026,"Display":"2026 Indoor"}],"eventsTF":[{"IDEvent":1,"Event":"60 Meters","PersonalEvent":true}],"grades":{"3204_12026":11},"meets":{"1":{"MeetName":"XC Meet","EndDate":"2026-09-01T00:00:00"}},"resultsTF":[{"IDResult":1,"SchoolID":3204,"SeasonID":12026,"EventID":1,"Result":"7.90a"}],"resultsXC":[{"IDResult":2,"SchoolID":3204,"SeasonID":2026,"MeetID":1,"Distance":5000,"Result":"19:23.40","PersonalBest":1}]});
    let page = parse(
        &athlete_target("42", None, Sport::CrossCountry)?,
        &body.to_string(),
    )?;
    assert_eq!(page.performances.len(), 2);
    assert_eq!(page.performances[0].sport, Sport::Indoor);
    assert_eq!(page.performances[0].season, Some(12026));
    assert_eq!(page.performances[1].sport, Sport::CrossCountry);
    assert_eq!(page.performances[1].event, "Cross Country 5000m");
    assert_eq!(page.performances[1].mark, "19:23.40");
    assert_eq!(page.performances[1].date.as_deref(), Some("2026-09-01"));
    assert!(!page.performances[1].is_relay);
    Ok(())
}

#[test]
fn given_unknown_or_wrong_identity_bodies_when_parsed_then_schema_errors_are_explicit() -> Result<()>
{
    let athlete = athlete_target("1", None, Sport::Outdoor)?;
    let wrong = parse(&athlete, BIO);
    let bad_json = parse(&athlete, "<html>denied</html>");
    let unknown = parse(&meet_target("634313", Sport::Outdoor)?, "{}");
    assert!(wrong.unwrap_err().to_string().contains("identity mismatch"));
    assert!(bad_json.unwrap_err().to_string().contains("not JSON"));
    assert!(unknown.unwrap_err().to_string().contains("schema mismatch"));
    assert!(athlete_target("0", None, Sport::Outdoor).is_err());
    assert!(seeds(&["XX".to_owned()], 2026).is_err());
    Ok(())
}

#[test]
fn given_relay_without_legs_when_parsed_then_team_identity_is_withheld_as_a_person() -> Result<()> {
    let metadata = parse(&meet_target("634313", Sport::Outdoor)?, MEET)?;
    let body = json!({"teams":[],"flatEvents":[{"Event":"4x100 Relay","Gender":"M","isPersonal":false,"results":[{"IDResult":1,"AthleteID":999,"FirstName":"Team A","Result":"42.00a","TeamID":3204,"SchoolName":"Middleton"}]}],"relayLegs":[]});
    let page = parse(&metadata.next[0], &body.to_string())?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert_eq!(page.unassigned_results.len(), 1);
    assert_eq!(page.unassigned_results[0].mark, "42.00a");
    assert!(page.unassigned_results[0].members.is_empty());
    assert!(
        page.notes
            .iter()
            .any(|n| n.contains("no published athlete legs"))
    );
    assert_eq!(page.schools[0].name, "Middleton");
    Ok(())
}

#[test]
fn given_status_and_unidentified_rows_when_parsed_then_every_published_result_is_retained()
-> Result<()> {
    let metadata = parse(&meet_target("634313", Sport::Outdoor)?, MEET)?;
    let body = json!({"teams":[],"relayLegs":[],"flatEvents":[{"Event":"100 Meters","Gender":"F","isPersonal":true,"results":[
        {"IDResult":1,"AthleteID":42,"FirstName":"Public","LastName":"Runner","Result":"DNS","TeamID":3204},
        {"IDResult":2,"AthleteID":42,"FirstName":"Public","LastName":"Runner","Result":"DNF","TeamID":3204},
        {"IDResult":3,"AthleteID":42,"FirstName":"Public","LastName":"Runner","Result":"DQ","TeamID":3204},
        {"IDResult":4,"Result":"11.90a","TeamID":3204}
    ]}]});
    let page = parse(&metadata.next[0], &body.to_string())?;
    assert_eq!(
        page.performances
            .iter()
            .map(|p| p.mark.as_str())
            .collect::<Vec<_>>(),
        ["DNS", "DNF", "DQ"]
    );
    assert_eq!(page.unassigned_results.len(), 1);
    assert_eq!(page.unassigned_results[0].result_id.as_deref(), Some("4"));
    assert_eq!(page.unassigned_results[0].mark, "11.90a");
    assert_eq!(page.unassigned_results[0].raw["IDResult"], 4);
    assert_eq!(page.performances.len() + page.unassigned_results.len(), 4);
    Ok(())
}

#[test]
fn given_private_bio_identity_when_parsed_then_result_rows_survive_without_a_fake_athlete()
-> Result<()> {
    let body = json!({"athlete":{"IDAthlete":42},"resultsTF":[{"IDResult":5,"Result":"DNS","EventID":1}],"eventsTF":[{"IDEvent":1,"Event":"100 Meters"}]});
    let page = parse(
        &athlete_target("42", None, Sport::Outdoor)?,
        &body.to_string(),
    )?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert_eq!(page.unassigned_results.len(), 1);
    assert_eq!(page.unassigned_results[0].mark, "DNS");
    assert_eq!(page.unassigned_results[0].event, "100 Meters");
    assert_eq!(page.unassigned_results[0].raw["IDResult"], 5);
    Ok(())
}
