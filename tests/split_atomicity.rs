use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use sha2::{Digest, Sha256};
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
        profile_details: Vec::new(),
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
fn existing_later_part_is_preserved_and_nothing_else_is_published() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("export.xlsx");
    let existing = directory.path().join("export-0002.xlsx");
    fs::write(&existing, b"other user's file")?;
    let records = [
        athlete(1, "First".to_owned()),
        athlete(2, "Second".to_owned()),
    ];

    assert!(write_spreadsheets(&output, &records, &options()).is_err());
    assert_eq!(fs::read(&existing)?, b"other user's file");
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[test]
fn successful_split_publishes_verified_parts_without_temporary_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("export.xlsx");
    let records = [
        athlete(1, "First".to_owned()),
        athlete(2, "Second".to_owned()),
    ];

    let parts = write_spreadsheets(&output, &records, &options())?;
    assert_eq!(
        parts,
        vec![
            directory.path().join("export-0001.xlsx"),
            directory.path().join("export-0002.xlsx")
        ]
    );
    let manifest_path = output.with_extension("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    assert_eq!(manifest["format"], "track-scrapper-export-v1");
    assert_eq!(manifest["complete"], true);
    let entries = manifest["parts"].as_array().context("completion manifest parts")?;
    let manifest_parts = entries.iter().map(|entry| {
        Ok(directory.path().join(entry["file"].as_str().context("manifest part name")?))
    }).collect::<Result<Vec<_>>>()?;
    assert_eq!(manifest_parts, parts);
    for ((part, name), entry) in parts.iter().zip(["First", "Second"]).zip(entries) {
        let bytes = fs::read(part)?;
        assert_eq!(entry["bytes"], u64::try_from(bytes.len())?);
        assert_eq!(entry["sha256"], format!("{:x}", Sha256::digest(&bytes)));
        let mut workbook = open_workbook_auto(part)?;
        let sheet = workbook.worksheet_range("Athletes")?;
        assert_eq!(
            sheet.get_value((1, 1)).map(ToString::to_string).as_deref(),
            Some(name)
        );
    }
    for entry in fs::read_dir(directory.path())? {
        let path = entry?.path();
        assert!(
            parts.contains(&path) || path == manifest_path,
            "unexpected unpublished or staging artifact: {}",
            path.display()
        );
    }
    Ok(())
}
