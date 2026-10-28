#[path = "support/records.rs"]
mod records;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    export::{ExportLayout, ExportOptions, StagedExport},
    model::{AthleteRecord, EvidencedPerformance, Source},
};

fn athlete(id: i64, event: &str, mark: &str) -> AthleteRecord {
    let mut performance = records::performance(mark);
    performance.event = event.into();
    performance.source_values = Some(serde_json::json!({"level":"all"}));
    AthleteRecord {
        id,
        name: format!("Athlete {id}"),
        gender: None,
        grade: None,
        graduation_year: Some(2027),
        needs_review: false,
        schools: Vec::new(),
        coaches: Vec::new(),
        profiles: Vec::new(),
        performances: vec![EvidencedPerformance {
            source: Source::MileSplit,
            url: format!("https://wi.milesplit.com/athletes/{id}"),
            performance,
        }],
        profile_details: Vec::new(),
    }
}

fn options(rows_per_file: usize) -> ExportOptions {
    ExportOptions {
        states: vec!["WI".into()],
        graduation_years: vec![2027],
        rows_per_file,
        per_athlete_sheets: true,
    }
}

#[test]
fn two_batches_publish_one_athletes_sheet_with_global_pr_columns_and_one_manifest_entry()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("all-athletes.xlsx");
    let mut export =
        StagedExport::new(&path, &options(1000))?.with_layout(ExportLayout::SingleSheet);
    export.stage_part(&[athlete(11, "100m", "11.00")], &[])?;
    export.stage_part(&[athlete(22, "800m", "2:00.00")], &[])?;
    export.stage_part(&[], &[])?;
    assert!(!path.exists());
    assert!(!path.with_extension("manifest.json").exists());
    assert_eq!(export.finish()?, vec![path.clone()]);
    let mut workbook = open_workbook_auto(&path)?;
    assert_eq!(workbook.sheet_names(), &["Athletes"]);
    let sheet = workbook.worksheet_range("Athletes")?;
    assert_eq!(sheet.height(), 3);
    let headers = sheet.rows().next().context("headers")?;
    let sprint = headers
        .iter()
        .position(|value| value.to_string() == "Outdoor 100m PR [all levels; milesplit]")
        .context("sprint PR column")?;
    let distance = headers
        .iter()
        .position(|value| value.to_string() == "Outdoor 800m PR [all levels; milesplit]")
        .context("distance PR column")?;
    let first = sheet.rows().nth(1).context("first athlete")?;
    let second = sheet.rows().nth(2).context("second athlete")?;
    assert_eq!(first[0].to_string(), "Athlete 11");
    assert_eq!(second[0].to_string(), "Athlete 22");
    assert_eq!(first[sprint].to_string(), "11.00 (2026-05-15)");
    assert_eq!(second[distance].to_string(), "2:00.00 (2026-05-15)");
    assert_eq!(first[distance].to_string(), "");
    assert_eq!(second[sprint].to_string(), "");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("manifest.json"))?)?;
    assert_eq!(manifest["complete"], true);
    let parts = manifest["parts"].as_array().context("manifest parts")?;
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["file"], "all-athletes.xlsx");
    let bytes = std::fs::read(&path)?;
    assert_eq!(parts[0]["bytes"], u64::try_from(bytes.len())?);
    assert_eq!(parts[0]["sha256"], format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 2);
    Ok(())
}

#[test]
fn total_single_sheet_row_limit_failure_cannot_publish_a_partial_manifest() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("all-athletes.xlsx");
    let mut export = StagedExport::new(&path, &options(1))?.with_layout(ExportLayout::SingleSheet);
    export.stage_part(&[athlete(11, "100m", "11.00")], &[])?;
    let error = export
        .stage_part(&[athlete(22, "800m", "2:00.00")], &[])
        .err()
        .context("row limit must fail")?;
    assert!(error.to_string().contains("configured row capacity"));
    assert!(export.finish().is_err());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}
