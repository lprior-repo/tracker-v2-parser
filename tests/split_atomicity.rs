use anyhow::Result;
use calamine::{Reader, open_workbook_auto};
use std::fs;
use track_scrapper_v2::export::{ExportOptions, write_spreadsheets};
use track_scrapper_v2::model::{AthleteRecord, School};

fn athlete(id: i64, name: String) -> AthleteRecord {
    AthleteRecord {
        id,
        name,
        gender: None,
        grade: None,
        graduation_year: Some(2028),
        needs_review: false,
        schools: vec![School {
            state: Some("OR".to_owned()),
            ..School::default()
        }],
        coaches: Vec::new(),
        profiles: Vec::new(),
        performances: Vec::new(),
    }
}

fn options() -> ExportOptions {
    ExportOptions {
        states: vec!["OR".to_owned()],
        graduation_years: vec![2028],
        rows_per_file: 1,
        per_athlete_sheets: false,
    }
}

#[test]
fn later_part_write_failure_leaves_no_published_or_temporary_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("export.xlsx");
    let records = [athlete(1, "First".to_owned()), athlete(2, "x".repeat(32_768))];

    assert!(write_spreadsheets(&output, &records, &options()).is_err());
    assert_eq!(fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn existing_later_part_is_preserved_and_nothing_else_is_published() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("export.xlsx");
    let existing = directory.path().join("export-0002.xlsx");
    fs::write(&existing, b"other user's file")?;
    let records = [athlete(1, "First".to_owned()), athlete(2, "Second".to_owned())];

    assert!(write_spreadsheets(&output, &records, &options()).is_err());
    assert_eq!(fs::read(&existing)?, b"other user's file");
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[test]
fn successful_split_publishes_verified_parts_without_temporary_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("export.xlsx");
    let records = [athlete(1, "First".to_owned()), athlete(2, "Second".to_owned())];

    let parts = write_spreadsheets(&output, &records, &options())?;
    assert_eq!(parts, vec![directory.path().join("export-0001.xlsx"), directory.path().join("export-0002.xlsx")]);
    for (part, name) in parts.iter().zip(["First", "Second"]) {
        let mut workbook = open_workbook_auto(part)?;
        let sheet = workbook.worksheet_range("Athletes")?;
        assert_eq!(sheet.get_value((1, 1)).map(ToString::to_string).as_deref(), Some(name));
    }
    assert_eq!(fs::read_dir(directory.path())?.count(), 2);
    Ok(())
}
