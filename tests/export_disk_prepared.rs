use std::path::Path;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::json;
use track_scrapper_v2::export::{
    ExportLayout, ExportOptions, PreparedChunk, StagedExport, verify_chunk_source,
};

fn options(rows: usize) -> ExportOptions {
    ExportOptions {
        states: vec!["WI".into()],
        graduation_years: vec![2027],
        rows_per_file: rows,
        per_athlete_sheets: false,
    }
}

fn chunk(first: i64, count: i64) -> Result<PreparedChunk> {
    let ids = (first..first + count).collect::<Vec<_>>();
    let rows = ids.iter().map(|id| {
        let mut prs = (0..12).map(|event| (format!("Event {event:02} PR"), json!([format!("{id}.{event:02} (2026-05-15)")]))).collect::<serde_json::Map<_, _>>();
        if *id == 20_000 { prs.insert("ZZ late event PR".into(), json!(["2:00.00"])); }
        json!({"identity": [format!("Athlete {id}"), "M", "2027", "Published School", "10 Main St, Madison, WI 53701", "Track Coach", "track@example.edu", "XC Coach", "xc@example.edu", "school@example.edu", format!("https://wi.milesplit.com/athletes/{id}"), id.to_string(), "Track Coach; XC Coach", "track@example.edu; xc@example.edu"], "prs": prs})
    }).collect::<Vec<_>>();
    Ok(serde_json::from_value(
        json!({"athlete_ids": ids, "rows": rows}),
    )?)
}

fn spools(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    std::fs::read_dir(path)?
        .filter_map(|entry| match entry {
            Ok(entry)
                if entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".prepared.jsonl") =>
            {
                Some(Ok(entry.path()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

#[test]
fn statewide_prepared_rows_exceed_old_ram_budget_and_publish_one_stream_verified_workbook()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("statewide.xlsx");
    let mut export = StagedExport::new(&path, &options(20_000))?
        .with_layout(ExportLayout::SingleSheet)
        .with_review(Vec::new())?;
    for index in 0..40 {
        export.stage_prepared(chunk(index * 500 + 1, 500)?)?;
    }
    assert!(!path.exists());
    assert!(!path.with_extension("manifest.json").exists());
    let files = spools(directory.path())?;
    assert_eq!(files.len(), 1);
    assert!(std::fs::metadata(&files[0])?.len() < 32 * 1024 * 1024);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&files[0])?.permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(export.finish()?, vec![path.clone()]);
    assert!(spools(directory.path())?.is_empty());
    let (rows, columns) = verify_chunk_source(
        &path,
        40,
        |index| chunk(i64::try_from(index)? * 500 + 1, 500),
        Some(&[]),
    )?;
    assert_eq!((rows, columns), (20_000, 27));
    let mut book = open_workbook_auto(&path)?;
    assert_eq!(book.sheet_names(), &["Athletes", "Review"]);
    let sheet = book.worksheet_range("Athletes")?;
    let headers = sheet
        .rows()
        .next()
        .context("headers")?
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        &headers[14..26],
        &(0..12)
            .map(|event| format!("Event {event:02} PR"))
            .collect::<Vec<_>>()
    );
    assert_eq!(headers[26], "ZZ late event PR");
    let first = sheet.rows().nth(1).context("first athlete")?;
    let last = sheet.rows().nth(20_000).context("last athlete")?;
    assert_eq!(first[0].to_string(), "Athlete 1");
    assert_eq!(first[6].to_string(), "track@example.edu");
    assert_eq!(first[8].to_string(), "xc@example.edu");
    assert_eq!(first[14].to_string(), "1.00 (2026-05-15)");
    assert_eq!(first[26].to_string(), "");
    assert_eq!(last[0].to_string(), "Athlete 20000");
    assert_eq!(last[26].to_string(), "2:00.00");
    assert_eq!(book.worksheet_range("Review")?.height(), 1);
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 2);
    Ok(())
}

#[test]
fn owned_spool_is_removed_on_drop_and_failed_staging_without_publishing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("abandoned.xlsx");
    let mut export = StagedExport::new(&path, &options(1))?.with_layout(ExportLayout::SingleSheet);
    export.stage_prepared(chunk(1, 1)?)?;
    assert_eq!(spools(directory.path())?.len(), 1);
    drop(export);
    assert!(spools(directory.path())?.is_empty());
    let mut export = StagedExport::new(&path, &options(1))?.with_layout(ExportLayout::SingleSheet);
    export.stage_prepared(chunk(1, 1)?)?;
    assert!(export.stage_prepared(chunk(2, 1)?).is_err());
    assert!(spools(directory.path())?.is_empty());
    assert!(export.finish().is_err());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn streaming_verification_rejects_changed_late_cells_missing_and_extra_rows() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("small.xlsx");
    let mut export = StagedExport::new(&path, &options(10))?.with_layout(ExportLayout::SingleSheet);
    export.stage_prepared(chunk(1, 2)?)?;
    export.stage_prepared(chunk(3, 2)?)?;
    export.finish()?;
    assert_eq!(
        verify_chunk_source(
            &path,
            2,
            |index| chunk(i64::try_from(index)? * 2 + 1, 2),
            None
        )?,
        (4, 26)
    );
    assert!(
        verify_chunk_source(
            &path,
            2,
            |index| chunk(if index == 1 { 4 } else { 1 }, 2),
            None
        )
        .is_err()
    );
    assert!(verify_chunk_source(&path, 1, |_| chunk(1, 2), None).is_err());
    assert!(
        verify_chunk_source(
            &path,
            3,
            |index| chunk(i64::try_from(index)? * 2 + 1, 2),
            None
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn bounded_caps_reject_oversized_prepared_rows_and_batches_before_publication() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, batch) in [
        ("row", {
            let mut value = serde_json::to_value(chunk(1, 1)?)?;
            value["rows"][0]["identity"][0] = json!("x".repeat(8 * 1024 * 1024));
            serde_json::from_value::<PreparedChunk>(value)?
        }),
        ("batch", {
            let prs = (0..140)
                .map(|index| (format!("PR {index}"), json!(["1.00"])))
                .collect::<serde_json::Map<_, _>>();
            let mut value = serde_json::to_value(chunk(1, 1000)?)?;
            for row in value["rows"].as_array_mut().context("prepared rows")? {
                row["prs"] = json!(prs);
            }
            serde_json::from_value::<PreparedChunk>(value)?
        }),
    ] {
        let path = directory.path().join(format!("{name}.xlsx"));
        let mut export =
            StagedExport::new(&path, &options(1000))?.with_layout(ExportLayout::SingleSheet);
        let error = export
            .stage_prepared(batch)
            .err()
            .context("bound must reject oversized input")?;
        assert!(
            error
                .to_string()
                .contains(if name == "row" { "8 MiB" } else { "128 MiB" })
        );
        assert!(export.finish().is_err());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    }
    Ok(())
}
