use super::{ExportOptions, write_spreadsheets};
use crate::marks::tests::evidence as numeric_evidence;
use crate::model::{AthleteRecord, Coach, School};
use calamine::{Data, Reader, Xlsx, open_workbook};
use std::collections::BTreeMap;

fn evidence(event: &str, mark: &str) -> crate::model::EvidencedPerformance {
    let mut evidence = numeric_evidence(event, mark);
    evidence.performance.source_values = Some(serde_json::json!({"level": "hs"}));
    evidence
}

fn athlete() -> AthleteRecord {
    AthleteRecord {
        id: 42,
        name: "=HYPERLINK(\"evil\")".to_owned(),
        gender: Some("Male".to_owned()),
        grade: Some("11".to_owned()),
        graduation_year: Some(2028),
        needs_review: false,
        profile_details: Vec::new(),
        schools: vec![School {
            id: "school1".to_owned(),
            name: "+School, North".to_owned(),
            state: Some("OR".to_owned()),
            address: Some("123 Main St".to_owned()),
            city: Some("Salem".to_owned()),
            postal_code: Some("97301".to_owned()),
            public_email: Some("office@school.test".to_owned()),
            website: None,
            uuid: None,
        }],
        coaches: vec![Coach {
            school_id: "school1".to_owned(),
            name: None,
            role: "Head".to_owned(),
            email: Some("@evil.test".to_owned()),
            sport: None,
            review_notes: Vec::new(),
        }],
        profiles: vec![
            "https://www.athletic.net/athlete/123".to_owned(),
            "https://www.milesplit.com/athletes/456".to_owned(),
        ],
        performances: vec![
            evidence("100m", "10.6a"),
            evidence("100m", "10.4a"),
            evidence("200m", "22.0a"),
            evidence("Long Jump", "20-6"),
            evidence("Long Jump", "6.30m"),
            evidence("Mystery", "=1+1"),
        ],
    }
}

fn options() -> ExportOptions {
    ExportOptions {
        states: vec!["OR".to_owned()],
        graduation_years: vec![2028],
        rows_per_file: 100,
        per_athlete_sheets: false,
    }
}

fn range(path: &std::path::Path, sheet: &str) -> anyhow::Result<calamine::Range<Data>> {
    let mut workbook: Xlsx<_> = open_workbook(path)?;
    Ok(workbook.worksheet_range(sheet)?)
}

fn row(range: &calamine::Range<Data>) -> anyhow::Result<BTreeMap<String, String>> {
    let mut rows = range.rows();
    let headers = rows
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing header row"))?;
    let values = rows
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing athlete row"))?;
    Ok(headers
        .iter()
        .zip(values)
        .map(|(header, value)| (header.to_string(), value.to_string()))
        .collect())
}

#[test]
fn given_many_events_when_exporting_then_one_literal_string_row_contains_bests_and_sources()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("events.xlsx");
    assert_eq!(
        write_spreadsheets(&destination, &[athlete()], &options())?,
        vec![destination.clone()]
    );
    let athletes = range(&destination, "Athletes")?;
    assert_eq!(athletes.height(), 2);
    assert_eq!(
        athletes.get_value((1, 1)),
        Some(&Data::String("=HYPERLINK(\"evil\")".to_owned()))
    );
    let values = row(&athletes)?;
    assert_eq!(values["school_names"], "+School, North");
    assert_eq!(values["coach_emails"], "@evil.test");
    assert_eq!(values["school_public_emails"], "office@school.test");
    assert!(values["school_postal_addresses"].contains("123 Main St"));
    assert_eq!(
        values["best_observed::athleticnet::outdoor::100m::fat::unknown::mark"],
        "10.4a"
    );
    assert_eq!(
        values["best_observed::athleticnet::outdoor::200m::fat::unknown::mark"],
        "22.0a"
    );
    assert_eq!(
        values["best_observed::athleticnet::outdoor::long_jump::non_time::unknown::mark"],
        "6.30m"
    );
    assert_eq!(values["unmapped_count"], "1");
    assert_eq!(range(&destination, "Issues")?.height(), 3);
    let mut workbook: Xlsx<_> = open_workbook(&destination)?;
    let profiles = workbook.hyperlinks_by_sheet_name("Profile_details")?;
    let targets = profiles
        .into_iter()
        .filter_map(|link| link.target)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(targets, athlete().profiles.into_iter().collect());
    assert_eq!(
        values["source_profiles"],
        "https://www.athletic.net/athlete/123; https://www.milesplit.com/athletes/456"
    );
    Ok(())
}

#[test]
fn given_uncertain_year_state_or_identity_when_exporting_then_source_rows_remain_visible_and_flagged()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("issues.xlsx");
    let mut unknown = athlete();
    unknown.graduation_year = None;
    let mut unresolved = athlete();
    unresolved.id = 43;
    unresolved.needs_review = true;
    let mut outside = athlete();
    outside.id = 44;
    outside.graduation_year = Some(2027);
    let mut missing_state = athlete();
    missing_state.id = 45;
    missing_state.schools[0].state = None;
    write_spreadsheets(
        &destination,
        &[unknown, unresolved, outside, missing_state],
        &options(),
    )?;
    let athletes = range(&destination, "Athletes")?;
    let headers = athletes
        .rows()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing headers"))?;
    let mut by_id = BTreeMap::new();
    for values in athletes.rows().skip(1) {
        let fields = headers
            .iter()
            .zip(values)
            .map(|(header, value)| (header.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>();
        assert!(by_id.insert(fields["athlete_id"].clone(), fields).is_none());
    }
    assert_eq!(
        by_id.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["42", "43", "45"]
    );
    assert_eq!(by_id["42"]["graduation_year"], "");
    assert!(by_id["42"]["review_notes"].contains("unknown graduation year"));
    assert_eq!(by_id["45"]["school_states"], "");
    assert!(by_id["45"]["review_notes"].contains("missing published state"));
    assert!(by_id["43"]["review_notes"].contains("unresolved identity"));
    assert!(
        by_id
            .values()
            .all(|fields| fields["needs_review"] == "true")
    );
    assert!(
        by_id
            .values()
            .all(|fields| fields.values().any(|value| value == "10.4a"))
    );
    let issues = range(&destination, "Issues")?;
    assert_eq!(issues.height(), 7);
    assert!(
        issues
            .rows()
            .flatten()
            .any(|cell| cell.to_string().contains("unknown graduation year"))
    );
    assert!(
        issues
            .rows()
            .flatten()
            .any(|cell| cell.to_string().contains("unresolved identity"))
    );
    Ok(())
}

#[test]
fn given_existing_target_when_exporting_then_original_bytes_are_not_overwritten()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("existing.xlsx");
    std::fs::write(&destination, "original user artifact")?;
    assert!(write_spreadsheets(&destination, &[athlete()], &options()).is_err());
    assert_eq!(
        std::fs::read_to_string(&destination)?,
        "original user artifact"
    );
    Ok(())
}

#[test]
fn given_split_and_detail_options_when_exporting_then_each_file_uses_same_queried_records()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("split.xlsx");
    let mut second = athlete();
    second.id = 43;
    let mut options = options();
    options.rows_per_file = 1;
    options.per_athlete_sheets = true;
    let files = write_spreadsheets(&destination, &[athlete(), second], &options)?;
    assert_eq!(files.len(), 2);
    assert!(!destination.exists());
    files.iter().enumerate().try_for_each(|(index, file)| {
        assert_eq!(range(file, "Athletes")?.height(), 2);
        assert_eq!(range(file, &format!("Athlete_{}", 42 + index))?.height(), 7);
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

#[test]
fn given_empty_snapshot_when_exporting_then_both_required_sheets_still_exist() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("empty.xlsx");
    assert_eq!(
        write_spreadsheets(&destination, &[], &options())?,
        vec![destination.clone()]
    );
    assert_eq!(range(&destination, "Athletes")?.height(), 1);
    assert_eq!(range(&destination, "Issues")?.height(), 1);
    Ok(())
}

#[test]
fn given_numeric_disqualified_result_when_exported_then_published_status_and_mark_remain_visible()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("reported-status.xlsx");
    let mut record = athlete();
    let mut disqualified = evidence("100m", "9.99a");
    disqualified.performance.reported_status = Some("DQ".to_owned());
    record.performances.push(disqualified);
    let mut options = options();
    options.per_athlete_sheets = true;
    write_spreadsheets(&destination, &[record], &options)?;
    let main = row(&range(&destination, "Athletes")?)?;
    assert_eq!(
        main["best_observed::athleticnet::outdoor::100m::fat::unknown::mark"],
        "10.4a"
    );
    assert!(main["review_notes"].contains("Issues"));
    let issues = range(&destination, "Issues")?;
    assert!(
        issues
            .rows()
            .flatten()
            .any(|cell| cell.to_string().contains("9.99a") && cell.to_string().contains("DQ"))
    );
    assert_eq!(main["needs_review"], "true");
    let detail = range(&destination, "Athlete_42")?;
    let mut rows = detail.rows();
    let headers = rows
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing detail headers"))?;
    let fields = rows
        .map(|values| {
            headers
                .iter()
                .zip(values)
                .map(|(header, value)| (header.to_string(), value.to_string()))
                .collect::<BTreeMap<_, _>>()
        })
        .find(|values| values.get("raw_mark").is_some_and(|mark| mark == "9.99a"))
        .ok_or_else(|| anyhow::anyhow!("missing disqualified observation"))?;
    assert_eq!(fields["reported_status"], "DQ");
    assert!(fields["comparison_context"].contains("disqualification"));
    Ok(())
}

#[test]
fn given_complete_contact_when_review_annotations_exported_then_identity_is_preserved_and_review_is_required()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("contact-annotations.xlsx");
    let mut record = athlete();
    record.performances.clear();
    record.coaches[0].name = Some("Ada Runner".into());
    record.coaches[0].email = Some("ada@school.test".into());
    assert!(!super::selection::requires_review(&record));
    record.coaches[0].review_notes = vec![
        "Affiliation is uncertain".into(),
        "Current tenure is not confirmed".into(),
    ];
    let long_note = "published contact note ".repeat(2_000);
    record.coaches[0].review_notes.push(long_note.clone());
    assert!(super::selection::requires_review(&record));
    write_spreadsheets(&destination, &[record], &options())?;
    let values = row(&range(&destination, "Athletes")?)?;
    assert_eq!(values["coach_names"], "Ada Runner");
    assert_eq!(values["coach_emails"], "ada@school.test");
    assert_eq!(values["needs_review"], "true");
    assert!(values["review_notes"].encode_utf16().count() <= 1_024);
    assert!(
        values["review_notes"]
            .contains("additional contact notes omitted; see Issues for complete details")
    );
    let issues = range(&destination, "Issues")?;
    assert!(issues.rows().skip(1).any(|row| {
        row.iter()
            .any(|cell| cell.to_string().contains("Affiliation is uncertain"))
    }));
    assert!(
        issues
            .rows()
            .flatten()
            .any(|cell| cell.to_string().contains("Current tenure is not confirmed"))
    );
    assert!(issues.rows().flatten().any(|cell| {
        cell.to_string()
            .contains("Ada Runner <ada@school.test> | school_id=school1 | Head")
    }));
    let complete_notes = issues
        .rows()
        .skip(1)
        .map(|row| row[3].to_string())
        .collect::<String>();
    assert!(complete_notes.contains(&long_note));
    Ok(())
}

#[test]
fn given_legacy_coach_json_when_deserialized_then_review_annotations_default_empty_and_are_not_source_facts()
-> anyhow::Result<()> {
    let published = serde_json::json!({
        "school_id": "school1",
        "name": "Ada Runner",
        "role": "Head",
        "email": "ada@school.test",
        "sport": null
    });
    let coach: Coach = serde_json::from_value(published.clone())?;
    assert_eq!(coach.review_notes, Vec::<String>::new());
    assert_eq!(serde_json::to_value(coach)?, published);
    Ok(())
}
