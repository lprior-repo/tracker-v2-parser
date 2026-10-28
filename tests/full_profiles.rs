mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::Value;
use track_scrapper_v2::{
    database::Database,
    export::{ExportOptions, write_spreadsheets},
    model::{Capture, PageContext, PageKind, Source, Target},
    sources,
};

const URL: &str = "https://oh.milesplit.com/athletes/15005189-brice-fuller";
const PROFILE: &str = include_str!("../fixtures/milesplit/athlete-oh-15005189-current.html");
const PROGRESSION: &str =
    include_str!("../fixtures/milesplit/athlete-oh-15005189-progression-current.html");

fn target(url: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        url.to_owned(),
        PageContext::default(),
    )
}

#[test]
fn given_real_public_profile_when_parsed_then_all_published_levels_survive() -> Result<()> {
    let page = sources::parse(&target(URL), PROFILE)?;
    assert_eq!(page.athletes[0].id, "15005189");
    assert_eq!(page.performances.len(), 28);
    let indoor = page
        .performances
        .iter()
        .filter(|row| row.event == "800m" && row.sport == track_scrapper_v2::model::Sport::Indoor)
        .map(|row| {
            (
                row.source_values
                    .as_ref()
                    .and_then(|v| v["level"].as_str())
                    .map(str::to_owned),
                row.mark.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        indoor,
        vec![
            (Some("all".to_owned()), "2:10.99"),
            (Some("hs".to_owned()), "2:11.88"),
            (Some("ms".to_owned()), "2:10.99")
        ]
    );
    let details = page.athletes[0]
        .source_values
        .as_ref()
        .context("profile evidence")?;
    assert_eq!(
        details["pages"][URL]["pr_rows"]
            .as_array()
            .context("PR rows")?
            .len(),
        33
    );
    assert_eq!(
        details["pages"][URL]["result_rows"]
            .as_array()
            .context("history rows")?
            .len(),
        99
    );
    assert_eq!(
        details["pages"][URL]["progression"][0]["1600m"]["mark"],
        "4:21.48"
    );
    assert!(
        page.next
            .iter()
            .any(|next| next.url == format!("{URL}/progression"))
    );
    Ok(())
}

#[tokio::test]
async fn given_real_profile_when_replayed_through_postgres_then_snapshot_exports_all_evidence()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for (url, body) in [
                (URL.to_owned(), PROFILE),
                (format!("{URL}/progression"), PROGRESSION),
                (URL.to_owned(), PROFILE),
            ] {
                let target = target(&url);
                let page = sources::parse(&target, body)?;
                let capture = Capture {
                    url,
                    status: 200,
                    content_type: "text/html".to_owned(),
                    body: body.as_bytes().to_vec(),
                };
                database.persist(None, &target, &capture, &page).await?;
            }
            let (records, _) = database
                .export_snapshot(&["OH".to_owned()], &[2028])
                .await?;
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].performances.len(), 28);
            assert!(
                records[0].needs_review,
                "contradictory published tab gender must remain flagged"
            );
            assert_eq!(records[0].gender.as_deref(), Some("male"));
            let details = &records[0].profile_details[0];
            assert_eq!(details.source_id, "15005189");
            assert_eq!(
                details.source_values["pages"]
                    .as_object()
                    .context("saved pages")?
                    .len(),
                2
            );
            let path = test.directory().join("full-profile.xlsx");
            write_spreadsheets(
                &path,
                &records,
                &ExportOptions {
                    states: vec!["OH".to_owned()],
                    graduation_years: vec![2028],
                    rows_per_file: 100,
                    per_athlete_sheets: true,
                },
            )?;
            let mut workbook = open_workbook_auto(&path)?;
            let athletes = workbook.worksheet_range("Athletes")?;
            assert_eq!(
                athletes.height(),
                2,
                "one header and exactly one canonical athlete row"
            );
            let header = athletes.rows().next().context("athlete headers")?;
            let row = athletes.rows().nth(1).context("Brice athlete row")?;
            let name_column = header
                .iter()
                .position(|cell| cell.to_string() == "name")
                .context("name column")?;
            assert_eq!(row[name_column].to_string(), "Brice Fuller");
            let id_column = header
                .iter()
                .position(|cell| cell.to_string() == "athlete_id")
                .context("identity column")?;
            assert_eq!(row[id_column].to_string(), records[0].id.to_string());
            for (event_column, expected_mark) in [
                ("::outdoor::1600m::", "4:12.69"),
                ("::outdoor::3200m::", "8:56.36"),
                ("::indoor::800m::", "2:11.88"),
                ("::xc::5000m::", "14:49.06"),
            ] {
                let columns = header
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| {
                        let name = cell.to_string();
                        name.starts_with("best_observed::milesplit::")
                            && name.contains(event_column)
                            && name.ends_with("::mark")
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                assert!(!columns.is_empty(), "missing event column {event_column}");
                assert!(
                    columns
                        .iter()
                        .any(|index| row[*index].to_string() == expected_mark),
                    "published {expected_mark} must occupy the same Brice row under {event_column}"
                );
            }
            let range = workbook.worksheet_range("Profile_details")?;
            let json = range
                .rows()
                .skip(1)
                .map(|row| row[6].to_string())
                .collect::<String>();
            assert_eq!(serde_json::from_str::<Value>(&json)?, details.source_values);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[test]
fn given_headline_only_pr_when_parsed_then_evidence_keeps_real_identity_without_inventing_sport()
-> Result<()> {
    let body = format!(
        "<link rel='canonical' href='{URL}'><h1 id='athleteName'>Brice Fuller</h1><table id='prTable'><tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>100m</td><td class='pr-td-mark'>DNF</td></tr></table>"
    );
    let page = sources::parse(&target(URL), &body)?;
    assert!(page.performances.is_empty());
    assert_eq!(page.athletes[0].id, "15005189");
    assert_eq!(
        page.athletes[0]
            .source_values
            .as_ref()
            .context("evidence")?["pages"][URL]["pr_rows"][0]["text"],
        "100m DNF"
    );
    assert!(sources::parse(&target(URL), &body.replace("15005189", "15005190")).is_err());
    Ok(())
}

#[test]
fn given_visible_history_and_status_pr_when_parsed_then_meet_context_and_status_survive()
-> Result<()> {
    let body = format!(
        "<link rel='canonical' href='{URL}'><h1 id='athleteName'>Brice Fuller</h1><table id='prTable'><tr data-pr-mode='outdoor' data-pr-level='ms'><td>100m</td><td>DNF</td><td>-</td><td>-</td><td>May 1, 2025</td></tr></table><div id='stats'><div class='record' data-season='outdoor' data-level='hs' data-event='1600m'><div class='seed'>4:12.69</div><div class='location'><a href='/meets/123-real-meet'>Real Meet</a></div><div class='date'>May 2, 2025</div></div></div>"
    );
    let page = sources::parse(&target(URL), &body)?;
    assert_eq!(page.performances.len(), 2);
    assert_eq!(page.performances[0].reported_status.as_deref(), Some("DNF"));
    assert_eq!(page.performances[1].meet_id.as_deref(), Some("123"));
    assert_eq!(page.performances[1].date.as_deref(), Some("2025-05-02"));
    assert_eq!(
        page.performances[1]
            .source_values
            .as_ref()
            .context("history context")?["meet_url"],
        "https://oh.milesplit.com/meets/123-real-meet"
    );
    assert!(sources::parse(&target(URL), &body.replace("data-pr-level='ms'", "")).is_err());
    assert!(
        sources::parse(
            &target(URL),
            &body.replace("<td>-</td><td>-</td>", "<td>-</td>")
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn given_excessive_profile_rows_when_parsed_then_no_silent_truncation_occurs() {
    let body = format!(
        "<link rel='canonical' href='{URL}'><h1 id='athleteName'>Brice Fuller</h1><table id='prTable'>{}</table>",
        "<tr data-pr-mode='all'><td>100m</td></tr>".repeat(10_001)
    );
    assert!(sources::parse(&target(URL), &body).is_err());
}

#[test]
fn given_real_profile_without_pr_table_when_parsed_then_public_identity_and_published_tabs_remain()
-> Result<()> {
    let url = "https://al.milesplit.com/athletes/15797488-baylee-bennett";
    let page = sources::parse(
        &target(url),
        include_str!("../fixtures/milesplit/athlete-al-15797488-current.html"),
    )?;
    assert_eq!(page.athletes[0].name, "Baylee Bennett");
    assert_eq!(page.athletes[0].id, "15797488");
    assert!(page.performances.is_empty());
    assert!(page.athletes[0].source_values.is_some());
    assert!(
        page.next
            .iter()
            .any(|next| next.url == format!("{url}/stats"))
    );
    assert!(
        page.next
            .iter()
            .any(|next| next.url == format!("{url}/progression"))
    );
    Ok(())
}

#[tokio::test]
async fn given_evidence_io_failure_when_replaying_then_checkpoint_fails_and_valid_replay_recovers()
-> Result<()> {
    use sha2::{Digest, Sha256};
    support::with_schema(|test| Box::pin(async move {
        let evidence_root = test.directory().join("evidence");
        std::fs::write(&evidence_root, b"not a directory")?;
        let fixture = support::fixture("milesplit/athlete-oh-15005189-current.html");
        let input = fixture.to_str().context("fixture path")?;
        let args = ["scrape", "--input", input, "--source", "milesplit", "--kind", "athlete", "--url", URL];
        let failed = test.cli(&args).await?;
        assert!(!failed.status.success());
        assert!(String::from_utf8_lossy(&failed.stderr).contains("retaining profile evidence"));
        let checkpoint: (String, i32) = sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE cursor->>'url'=$1")
            .bind(URL).fetch_one(&test.pool).await?;
        assert_eq!(checkpoint, ("failed".to_owned(), 1));
        let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(results, 0);
        std::fs::remove_file(&evidence_root)?;
        support::assert_success(&test.cli(&args).await?);
        let progression_url = format!("{URL}/progression");
        let progression = support::fixture("milesplit/athlete-oh-15005189-progression-current.html");
        support::assert_success(&test.cli(&["scrape", "--input", progression.to_str().context("progression path")?, "--source", "milesplit", "--kind", "athlete", "--url", &progression_url]).await?);
        support::assert_success(&test.cli(&args).await?);
        let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results").fetch_one(&test.pool).await?;
        assert_eq!(results, 28);
        let values: Value = sqlx::query_scalar("SELECT details->'source_values' FROM athlete_sources WHERE source='milesplit' AND source_id='15005189'")
            .fetch_one(&test.pool).await?;
        for (url, marker) in [(URL, "8:56.36"), (progression_url.as_str(), "\"gender\": \"Female\"")] {
            let capture = &values["pages"][url]["_capture"];
            let path = std::path::Path::new(capture["artifact_path"].as_str().context("retained artifact")?);
            assert!(path.starts_with(&evidence_root));
            let bytes = std::fs::read(path)?;
            assert_eq!(capture["sha256"].as_str(), Some(format!("{:x}", Sha256::digest(&bytes)).as_str()));
            assert_eq!(capture["bytes"].as_u64(), Some(bytes.len() as u64));
            assert_eq!(capture["source_url"], url);
            assert_eq!(capture["representation"], "transport_sanitized_response");
            assert!(std::str::from_utf8(&bytes)?.contains(marker));
        }
        Ok(())
    })).await
}
