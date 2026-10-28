#[path = "support/records.rs"]
mod records;
mod support;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::database::Database;
use track_scrapper_v2::export::{ExportOptions, write_spreadsheets_with_issues};
use track_scrapper_v2::model::{Athlete, ParsedPage, School, Source};

fn workbook_rows(path: &Path, sheet: &str) -> Result<Vec<BTreeMap<String, String>>> {
    let mut workbook = open_workbook_auto(path)?;
    let range = workbook.worksheet_range(sheet)?;
    let mut rows = range.rows();
    let headers = rows.next().context("missing workbook headers")?;
    Ok(rows
        .map(|row| {
            headers
                .iter()
                .zip(row)
                .map(|(header, value)| (header.to_string(), value.to_string()))
                .collect()
        })
        .collect())
}

async fn seed_athlete(
    database: &Database,
    id: &str,
    name: &str,
    state: &str,
    year: i32,
) -> Result<()> {
    let school_id = format!("school-{id}");
    let school = School {
        id: school_id.clone(),
        name: format!("{name} School"),
        state: Some(state.to_owned()),
        ..School::default()
    };
    let athlete = Athlete {
        name: name.to_owned(),
        school_id: Some(school_id),
        state: Some(state.to_owned()),
        graduation_year: Some(year),
        ..records::athlete(id)
    };
    records::persist(
        database,
        &records::target(Source::AthleticNet, id),
        &ParsedPage {
            schools: vec![school],
            athletes: vec![athlete],
            ..ParsedPage::default()
        },
    )
    .await
}

#[tokio::test]
async fn mixed_source_snapshot_exports_linked_bests_and_excludes_other_states_and_years()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let athletic_url = "https://www.athletic.net/athlete/11";
            let milesplit_url = "https://www.milesplit.com/athletes/21";
            let athletic = Athlete {
                school_id: Some("wi-school".to_owned()),
                profile_url: Some(athletic_url.to_owned()),
                ..records::athlete("11")
            };
            let school = School {
                id: "wi-school".to_owned(),
                name: "Wisconsin High".to_owned(),
                state: Some("WI".to_owned()),
                ..School::default()
            };
            let first = records::target(Source::AthleticNet, "11");
            let second = records::target(Source::MileSplit, "21");
            let mut slow = records::performance("11.20");
            slow.source_values = Some(serde_json::json!({"level": "hs"}));
            slow.result_id = Some("an-slow".to_owned());
            let mut fast = records::performance("10.90");
            fast.source_values = Some(serde_json::json!({"level": "hs"}));
            fast.result_id = Some("an-fast".to_owned());
            records::persist(
                &database,
                &first,
                &ParsedPage {
                    schools: vec![school],
                    athletes: vec![athletic.clone()],
                    performances: vec![slow, fast],
                    ..ParsedPage::default()
                },
            )
            .await?;
            let milesplit = Athlete {
                profile_url: Some(milesplit_url.to_owned()),
                ..records::athlete("21")
            };
            let mut mile_mark = records::performance("11.05");
            mile_mark.source_values = Some(serde_json::json!({"level": "hs"}));
            mile_mark.athlete_id = "21".to_owned();
            mile_mark.result_id = Some("ms-result".to_owned());
            records::persist(
                &database,
                &second,
                &ParsedPage {
                    athletes: vec![milesplit],
                    performances: vec![mile_mark],
                    ..ParsedPage::default()
                },
            )
            .await?;
            records::persist(
                &database,
                &first,
                &ParsedPage {
                    athletes: vec![Athlete {
                        same_as: vec![milesplit_url.to_owned()],
                        ..athletic
                    }],
                    ..ParsedPage::default()
                },
            )
            .await?;
            seed_athlete(&database, "31", "Ohio Runner", "OH", 2027).await?;
            seed_athlete(&database, "41", "Later Graduate", "WI", 2028).await?;
            let mut pending = records::target(Source::MileSplit, "pending");
            pending.context.state = Some("WI".to_owned());
            database.enqueue(&[pending.clone()], false).await?;

            let options = ExportOptions {
                states: vec!["WI".to_owned()],
                graduation_years: vec![2027],
                rows_per_file: 100,
                per_athlete_sheets: false,
            };
            let (snapshot, issues) = database
                .export_snapshot(&options.states, &options.graduation_years)
                .await?;
            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].name, "Alex Morgan");
            assert_eq!(
                snapshot[0].profiles,
                vec![athletic_url.to_owned(), milesplit_url.to_owned()]
            );
            assert_eq!(snapshot[0].performances.len(), 3);
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.url == pending.url && issue.detail.contains("page pending"))
            );
            let output = test.directory().join("mixed-source.xlsx");
            assert_eq!(
                write_spreadsheets_with_issues(&output, &snapshot, &issues, &options)?,
                vec![output.clone()]
            );
            let rows = workbook_rows(&output, "Athletes")?;
            assert_eq!(rows.len(), 1);
            let row = &rows[0];
            assert_eq!(row["name"], "Alex Morgan");
            assert_eq!(row["graduation_year"], "2027");
            assert_eq!(row["school_names"], "Wisconsin High");
            assert_eq!(row["school_states"], "WI");
            assert_eq!(
                row["source_profiles"],
                format!("{athletic_url}; {milesplit_url}")
            );
            assert_eq!(
                row["best_observed::athleticnet::outdoor::100m::fat::legal::mark"],
                "10.90"
            );
            assert_eq!(
                row["best_observed::athleticnet::outdoor::100m::fat::legal::source_url"],
                athletic_url
            );
            assert_eq!(
                row["best_observed::milesplit::outdoor::100m::fat::legal::mark"],
                "11.05"
            );
            assert_eq!(
                row["best_observed::milesplit::outdoor::100m::fat::legal::source_url"],
                milesplit_url
            );
            let issue_rows = workbook_rows(&output, "Issues")?;
            assert!(
                issue_rows
                    .iter()
                    .any(|row| row.values().any(|cell| cell.contains(&pending.url))
                        && row.values().any(|cell| cell.contains("page pending")))
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
