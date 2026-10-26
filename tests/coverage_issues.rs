#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::Result;
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::database::Database;
use track_scrapper_v2::export::{ExportOptions, write_spreadsheets_with_issues};
use track_scrapper_v2::model::{ParsedPage, Source, Sport, UnassignedResult};

#[tokio::test]
async fn given_incomplete_pages_and_unassigned_results_when_exported_then_issues_remain_visible()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let completed = records::target(Source::AthleticNet, "coverage-completed");
            let pending = records::target(Source::AthleticNet, "coverage-pending");
            let page = ParsedPage {
                notes: vec!["masked ranking rows remain unavailable".to_owned()],
                unassigned_results: vec![UnassignedResult {
                    result_id: Some("unassigned-1".to_owned()),
                    source_row: None,
                    event: "4x100 Relay".to_owned(),
                    mark: "45.20".to_owned(),
                    sport: Sport::Outdoor,
                    school_id: None,
                    meet_id: None,
                    date: None,
                    is_relay: true,
                    members: vec![],
                    raw: serde_json::json!({}),
                }],
                ..ParsedPage::default()
            };
            records::persist(&database, &completed, &page).await?;
            database.enqueue(&[pending.clone()], false).await?;
            let (athletes, issues) = database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            let destination = test.directory().join("coverage.xlsx");
            let options = ExportOptions {
                states: vec!["WI".to_owned()],
                graduation_years: vec![2027],
                rows_per_file: 100,
                per_athlete_sheets: false,
            };
            write_spreadsheets_with_issues(&destination, &athletes, &issues, &options)?;
            let mut workbook = open_workbook_auto(&destination)?;
            let sheet = workbook.worksheet_range("Issues")?;
            let rows: Vec<Vec<String>> = sheet
                .rows()
                .map(|row| row.iter().map(ToString::to_string).collect())
                .collect();
            assert!(rows.iter().any(|row| {
                row.iter().any(|value| value == &completed.url)
                    && row
                        .iter()
                        .any(|value| value.contains("masked ranking rows remain unavailable"))
            }));
            assert!(
                rows.iter()
                    .any(|row| row.iter().any(|value| value == &pending.url)
                        && row.iter().any(|value| value.contains("page pending")))
            );
            assert!(rows.iter().any(|row| {
                row.iter().any(|value| value == &completed.url)
                    && row
                        .iter()
                        .any(|value| value.contains("4x100 Relay") && value.contains("45.20"))
            }));
            records::persist(&database, &completed, &ParsedPage::default()).await?;
            let (_, refreshed) = database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            assert!(
                !refreshed
                    .iter()
                    .any(|issue| issue.detail.contains("masked ranking rows"))
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
