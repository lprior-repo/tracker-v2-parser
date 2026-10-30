use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::export::{
    ExportLayout, ExportOptions, ReviewRow, ReviewStatus, StagedExport, verify_chunks_with_review,
};

fn row(checkpoint_id: i64) -> ReviewRow {
    ReviewRow {
        checkpoint_id,
        source: "milesplit".into(),
        kind: "athlete".into(),
        state: "WI".into(),
        school: String::new(),
        school_source_id: String::new(),
        athlete: String::new(),
        athlete_source_id: checkpoint_id.to_string(),
        url: format!("https://wi.milesplit.com/athletes/{checkpoint_id}"),
        status: ReviewStatus::Succeeded,
        attempts: 1,
        durable_state: String::new(),
        retry_after: String::new(),
        error: "Public source progression is restricted; no public PR rows were captured.".into(),
        last_success: String::new(),
        created: String::new(),
        next_action: "Check alternate public results; do not retry restricted sections.".into(),
        ai_guidance: "Preserve captured evidence and exact source IDs.".into(),
    }
}

fn options() -> ExportOptions {
    ExportOptions {
        states: vec!["WI".into()],
        graduation_years: vec![2027],
        rows_per_file: 100_000,
        per_athlete_sheets: false,
    }
}

#[test]
fn review_above_ten_thousand_rows_is_written_reopened_and_verified_with_bounded_hard_limits()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("eleven-thousand-review-rows.xlsx");
    let rows = (1..=11_000_i64).map(row).collect::<Vec<_>>();
    StagedExport::new(&path, &options())?
        .with_layout(ExportLayout::SingleSheet)
        .with_review(rows.clone())?
        .finish()?;
    let mut workbook = open_workbook_auto(&path)?;
    assert_eq!(workbook.sheet_names(), ["Athletes", "Review"]);
    let review = workbook.worksheet_range("Review")?;
    assert_eq!((review.height(), review.width()), (11_001, 18));
    assert_eq!(
        review
            .get_value((1, 0))
            .context("first checkpoint")?
            .to_string(),
        "1"
    );
    assert_eq!(
        review
            .get_value((11_000, 0))
            .context("last checkpoint")?
            .to_string(),
        "11000"
    );
    assert_eq!(
        review
            .get_value((11_000, 9))
            .context("retained successful status")?
            .to_string(),
        "succeeded"
    );
    assert_eq!(verify_chunks_with_review(&path, &[], Some(&rows))?.0, 0);
    let ceiling = directory.path().join("above-review-row-ceiling.xlsx");
    let too_many = (1..=100_001_i64).map(row).collect::<Vec<_>>();
    let failure = StagedExport::new(&ceiling, &options())?
        .with_layout(ExportLayout::SingleSheet)
        .with_review(too_many)
        .err()
        .context("row hard ceiling must remain enforced")?;
    assert_eq!(failure.to_string(), "review exceeds 100000 checkpoint rows");
    assert!(!ceiling.exists());
    Ok(())
}

#[test]
fn review_serialized_storage_above_sixty_four_mib_fails_before_publication() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("above-review-byte-ceiling.xlsx");
    let rows = (1..=2200_i64)
        .map(|id| {
            let mut row = row(id);
            row.error = "x".repeat(32_000);
            row
        })
        .collect::<Vec<_>>();
    let failure = StagedExport::new(&path, &options())?
        .with_layout(ExportLayout::SingleSheet)
        .with_review(rows)
        .err()
        .context("storage hard ceiling must remain enforced")?;
    assert_eq!(
        failure.to_string(),
        "review exceeds 64 MiB storage capacity"
    );
    assert!(!path.exists());
    Ok(())
}
