use super::{ExportOptions, write_spreadsheets};
use crate::marks::tests::evidence;
use crate::model::{AthleteRecord, Coach, School};
use calamine::{Data, Reader, Xlsx, open_workbook};
use std::collections::BTreeMap;

fn athlete() -> AthleteRecord {
    AthleteRecord {
        id: 42,
        name: "=HYPERLINK(\"evil\")".to_owned(),
        gender: Some("Male".to_owned()),
        grade: Some("11".to_owned()),
        graduation_year: Some(2028),
        needs_review: false,
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
            email: "@evil.test".to_owned(),
            sport: None,
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

fn path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "track-scrapper-export-{}-{name}.xlsx",
        std::process::id()
    ))
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
    let destination = path("events");
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
    assert_eq!(range(&destination, "Issues")?.height(), 2);
    std::fs::remove_file(destination)?;
    Ok(())
}

#[test]
fn given_unknown_year_or_unresolved_identity_when_exporting_then_issues_not_guessed_rows()
-> anyhow::Result<()> {
    let destination = path("issues");
    let mut unknown = athlete();
    unknown.graduation_year = None;
    let mut unresolved = athlete();
    unresolved.id = 43;
    unresolved.needs_review = true;
    let mut outside = athlete();
    outside.id = 44;
    outside.graduation_year = Some(2027);
    write_spreadsheets(&destination, &[unknown, unresolved, outside], &options())?;
    assert_eq!(range(&destination, "Athletes")?.height(), 1);
    let issues = range(&destination, "Issues")?;
    assert_eq!(issues.height(), 3);
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
    std::fs::remove_file(destination)?;
    Ok(())
}

#[test]
fn given_existing_target_when_exporting_then_original_bytes_are_not_overwritten()
-> anyhow::Result<()> {
    let destination = path("existing");
    std::fs::write(&destination, "original user artifact")?;
    assert!(write_spreadsheets(&destination, &[athlete()], &options()).is_err());
    assert_eq!(
        std::fs::read_to_string(&destination)?,
        "original user artifact"
    );
    std::fs::remove_file(destination)?;
    Ok(())
}

#[test]
fn given_split_and_detail_options_when_exporting_then_each_file_uses_same_queried_records()
-> anyhow::Result<()> {
    let destination = path("split");
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
        std::fs::remove_file(file)?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

#[test]
fn given_empty_snapshot_when_exporting_then_both_required_sheets_still_exist() -> anyhow::Result<()>
{
    let destination = path("empty");
    assert_eq!(
        write_spreadsheets(&destination, &[], &options())?,
        vec![destination.clone()]
    );
    assert_eq!(range(&destination, "Athletes")?.height(), 1);
    assert_eq!(range(&destination, "Issues")?.height(), 1);
    std::fs::remove_file(destination)?;
    Ok(())
}
