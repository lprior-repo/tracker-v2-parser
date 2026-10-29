#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    database::Database,
    export::{ExportLayout, ExportOptions, StagedExport},
    model::{Athlete, Coach, ParsedPage, School, Sport},
};

fn page() -> ParsedPage {
    let performances = [
        ("all", "33.40", "200m"),
        ("hs", "34.00", "200m"),
        ("ms", "1:22.42", "440y"),
        ("all", "35.00", "200m"),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (level, mark, event))| {
        let mut row = records::performance(mark);
        row.event = event.into();
        row.source_values = Some(serde_json::json!({"level":level,"kind":"personal_record"}));
        row.result_id = Some(format!("pr-{i}"));
        row.sport = Sport::Indoor;
        row
    })
    .collect();
    let coaches = [
        ("Published Track Coach", "track@example.edu", None),
        (
            "Published XC Coach",
            "xc@example.edu",
            Some(Sport::CrossCountry),
        ),
    ]
    .into_iter()
    .map(|(name, email, sport)| Coach {
        school_id: "school".into(),
        name: Some(name.into()),
        role: if sport == Some(Sport::CrossCountry) {
            "Head XC Coach".into()
        } else {
            "Head Track Coach".into()
        },
        email: Some(email.into()),
        sport,
        review_notes: Vec::new(),
    })
    .collect();
    ParsedPage {
        athletes: vec![Athlete {
            id: "11".into(),
            name: "Published Athlete".into(),
            graduation_year: Some(2027),
            state: Some("WI".into()),
            school_id: Some("school".into()),
            ..Athlete::default()
        }],
        schools: vec![School {
            id: "school".into(),
            name: "Published High".into(),
            state: Some("WI".into()),
            address: Some("123 Published St".into()),
            ..School::default()
        }],
        performances,
        coaches,
        ..ParsedPage::default()
    }
}

#[tokio::test]
async fn given_all_level_prs_and_coaches_when_cli_exports_then_one_sheet_contains_every_pr_and_email()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let db=Database::connect(&test.url).await?;
        let target=records::target(track_scrapper_v2::model::Source::MileSplit,"11");
        records::persist(&db,&target,&page()).await?;
        let config=test.directory().join("single.toml");
        std::fs::write(&config,"states=['WI']\ngraduation_years=[2027]\n[export]\nlayout='single_sheet'\nper_athlete_sheets=true\n")?;
        let output=test.directory().join("athletes.xlsx");
        support::assert_success(&test.operation(&crate::support::OfflineOperation::Export {config:Some(std::path::PathBuf::from((config.to_str().context("config")?).to_string())),output:std::path::PathBuf::from((output.to_str().context("output")?).to_string()),per_athlete:false}).await?);
        let mut book=open_workbook_auto(&output)?;
        assert_eq!(book.sheet_names(), &["Athletes"]);
        let sheet=book.worksheet_range("Athletes")?;
        assert_eq!(sheet.height(),2);
        let headers=sheet.rows().next().context("headers")?;
        let row=sheet.rows().nth(1).context("athlete")?;
        let fields=headers.iter().zip(row).map(|(k,v)|(k.to_string(),v.to_string())).collect::<std::collections::BTreeMap<_,_>>();
        assert_eq!(fields["Track coach email"],"track@example.edu");
        assert_eq!(fields["XC coach email"],"xc@example.edu");
        assert_eq!(fields["Indoor 200m PR [all levels; milesplit]"],"33.40 (2026-05-15); 35.00 (2026-05-15)");
        assert_eq!(fields["Indoor 200m PR [hs; milesplit]"],"34.00 (2026-05-15)");
        assert_eq!(fields["Indoor 440y PR [ms; milesplit]"],"1:22.42 (2026-05-15)");
        assert!(!fields.keys().any(|k|k.contains("json")||k.contains("caveat")||k.contains("review")));
        let original=sheet.rows().map(|row|row.iter().map(ToString::to_string).collect::<Vec<_>>()).collect::<Vec<_>>();
        let (mut records,_) = db.export_snapshot(&["WI".into()], &[2027]).await?;
        records.iter_mut().for_each(|record| {record.performances.reverse();record.coaches.reverse();});
        let reordered=test.directory().join("reordered.xlsx");
        let options=ExportOptions{states:vec!["WI".into()],graduation_years:vec![2027],rows_per_file:1000,per_athlete_sheets:false};
        let mut export=StagedExport::new(&reordered,&options)?.with_layout(ExportLayout::SingleSheet);
        export.stage_part(&records,&[])?;
        export.finish()?;
        let mut reversed=open_workbook_auto(&reordered)?;
        let actual=reversed.worksheet_range("Athletes")?.rows().map(|row|row.iter().map(ToString::to_string).collect::<Vec<_>>()).collect::<Vec<_>>();
        assert_eq!(actual,original,"Reordering source claims must not change exported cells");
        db.close().await;
        Ok(())
    })).await
}

#[test]
fn given_empty_single_sheet_export_when_finished_then_only_one_empty_athletes_sheet_is_published()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("empty.xlsx");
    let options = ExportOptions {
        states: vec!["WI".into()],
        graduation_years: vec![2027],
        rows_per_file: 1000,
        per_athlete_sheets: true,
    };
    let mut export = StagedExport::new(&path, &options)?.with_layout(ExportLayout::SingleSheet);
    export.stage_part(&[], &[])?;
    export.stage_part(&[], &[])?;
    assert_eq!(export.finish()?, vec![path.clone()]);
    let mut book = open_workbook_auto(&path)?;
    assert_eq!(book.sheet_names(), &["Athletes"]);
    assert_eq!(book.worksheet_range("Athletes")?.height(), 1);
    assert!(path.with_extension("manifest.json").exists());
    Ok(())
}
