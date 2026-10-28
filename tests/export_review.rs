use anyhow::{Context, Result};
use calamine::{Data, Range, Reader, open_workbook_auto};
use std::collections::BTreeMap;
use track_scrapper_v2::export::{ExportIssue, ExportOptions, write_spreadsheets_with_issues};
use track_scrapper_v2::model::{
    AthleteRecord, EvidencedPerformance, PageContext, PageKind, School, Source, Sport, Target,
};
use track_scrapper_v2::sources;

fn options() -> ExportOptions {
    ExportOptions {
        states: vec!["OH".to_owned()],
        graduation_years: vec![2028],
        rows_per_file: 100,
        per_athlete_sheets: false,
    }
}

fn record(performances: Vec<EvidencedPerformance>) -> AthleteRecord {
    AthleteRecord {
        id: 42,
        name: "Brice Fuller".to_owned(),
        gender: Some("male".to_owned()),
        grade: Some("11".to_owned()),
        graduation_year: Some(2028),
        needs_review: false,
        schools: vec![School {
            id: "school".to_owned(),
            name: "Published High".to_owned(),
            state: Some("OH".to_owned()),
            ..School::default()
        }],
        coaches: Vec::new(),
        profiles: vec!["https://oh.milesplit.com/athletes/15005189-brice-fuller".to_owned()],
        performances,
        profile_details: Vec::new(),
    }
}

fn real_performances() -> Result<Vec<EvidencedPerformance>> {
    let url = "https://oh.milesplit.com/athletes/15005189-brice-fuller";
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        url.to_owned(),
        PageContext::default(),
    );
    Ok(sources::parse(
        &target,
        include_str!("../fixtures/milesplit/athlete-oh-15005189-current.html"),
    )?
    .performances
    .into_iter()
    .map(|performance| EvidencedPerformance {
        source: Source::MileSplit,
        url: url.to_owned(),
        performance,
    })
    .collect())
}

fn fields(range: &Range<Data>) -> Result<BTreeMap<String, String>> {
    let headers = range.rows().next().context("headers")?;
    let row = range.rows().nth(1).context("athlete row")?;
    Ok(headers
        .iter()
        .zip(row)
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect())
}

fn column(range: &Range<Data>, name: &str) -> Result<usize> {
    range
        .rows()
        .next()
        .context("headers")?
        .iter()
        .position(|value| value.to_string() == name)
        .with_context(|| format!("missing {name}"))
}

#[test]
fn given_unverified_prior_revision_when_exporting_then_it_stays_visible_but_cannot_win_until_reconfirmed()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let current = real_performances()?
        .into_iter()
        .find(|row| {
            row.performance.event == "800m"
                && row.performance.sport == Sport::Indoor
                && row
                    .performance
                    .source_values
                    .as_ref()
                    .and_then(|value| value.get("level"))
                    .and_then(serde_json::Value::as_str)
                    == Some("hs")
        })
        .context("published HS indoor800")?;
    let mut prior = current.clone();
    prior.performance.mark = "2:00.00".to_owned();
    let values = prior
        .performance
        .source_values
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
        .context("source evidence")?;
    values.insert(
        "revision_status".to_owned(),
        serde_json::json!("unverified_prior_revision"),
    );
    let path = directory.path().join("unverified.xlsx");
    write_spreadsheets_with_issues(
        &path,
        &[record(vec![prior.clone(), current])],
        &[],
        &options(),
    )?;
    let mut workbook = open_workbook_auto(&path)?;
    let main = fields(&workbook.worksheet_range("Athletes")?)?;
    assert!(
        main.iter()
            .any(|(key, value)| key.ends_with("::mark") && value == "2:11.88")
    );
    assert!(
        !main
            .iter()
            .any(|(key, value)| key.ends_with("::mark") && value == "2:00.00")
    );
    let observations = workbook.worksheet_range("Observations")?;
    assert_eq!(observations.height(), 3);
    let eligibility = column(&observations, "hs_pr_eligibility")?;
    assert!(observations.rows().skip(1).any(|row| {
        row[eligibility]
            .to_string()
            .contains("unverified prior revision")
    }));
    let issues = workbook.worksheet_range("Issues")?;
    assert!(issues.rows().skip(1).any(|row| {
        row[3]
            .to_string()
            .contains("not reconfirmed in the latest observed source file")
            && row[3].to_string().contains("2:00.00")
    }));
    let values = prior
        .performance
        .source_values
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
        .context("source evidence")?;
    assert_eq!(
        values.remove("revision_status"),
        Some(serde_json::json!("unverified_prior_revision"))
    );
    let reconfirmed = directory.path().join("reconfirmed.xlsx");
    write_spreadsheets_with_issues(&reconfirmed, &[record(vec![prior])], &[], &options())?;
    let mut workbook = open_workbook_auto(&reconfirmed)?;
    let main = fields(&workbook.worksheet_range("Athletes")?)?;
    assert!(
        main.iter()
            .any(|(key, value)| key.ends_with("::mark") && value == "2:00.00")
    );
    Ok(())
}

#[test]
fn given_real_mixed_levels_when_exporting_then_only_explicit_hs_wins_and_all_observations_remain()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("levels.xlsx");
    let mut observations = real_performances()?;
    let mut unknown = observations
        .iter()
        .find(|row| row.performance.event == "800m" && row.performance.sport == Sport::Indoor)
        .context("indoor800")?
        .clone();
    unknown.performance.source_values = None;
    unknown.performance.mark = "1:50.00".to_owned();
    observations.push(unknown.clone());
    unknown.performance.source_values = Some(serde_json::json!({"level":"college"}));
    unknown.performance.mark = "1:49.00".to_owned();
    observations.push(unknown.clone());
    unknown.performance.source_values =
        Some(serde_json::json!({"level":"hs", "row":{"attributes":{"data-pr-level":"ms"}}}));
    unknown.performance.mark = "1:48.00".to_owned();
    observations.push(unknown);
    let count = observations.len();
    write_spreadsheets_with_issues(&path, &[record(observations)], &[], &options())?;
    let mut workbook = open_workbook_auto(&path)?;
    let main = workbook.worksheet_range("Athletes")?;
    assert_eq!(main.height(), 2);
    let fields = fields(&main)?;
    let winners = fields
        .iter()
        .filter(|(key, _)| key.contains("::indoor::800m::") && key.ends_with("::mark"))
        .collect::<Vec<_>>();
    assert_eq!(winners.len(), 1);
    assert_eq!(winners[0].1, "2:11.88");
    let detail = workbook.worksheet_range("Observations")?;
    assert_eq!(detail.height(), count + 1);
    let mark = column(&detail, "raw_mark")?;
    let marks = detail
        .rows()
        .skip(1)
        .map(|row| row[mark].to_string())
        .collect::<Vec<_>>();
    for excluded in ["2:10.99", "1:50.00", "1:49.00", "1:48.00"] {
        assert!(marks.iter().any(|mark| mark == excluded));
    }
    assert!(
        workbook
            .worksheet_range("Issues")?
            .rows()
            .flatten()
            .any(|value| value.to_string().contains("college"))
    );
    Ok(())
}

fn course_observations(count: usize) -> Result<Vec<EvidencedPerformance>> {
    let template = real_performances()?
        .into_iter()
        .find(|row| {
            row.performance.sport == Sport::CrossCountry
                && row
                    .performance
                    .source_values
                    .as_ref()
                    .and_then(|value| value.get("level"))
                    .and_then(serde_json::Value::as_str)
                    == Some("hs")
        })
        .context("HS XC fixture")?;
    Ok((0..count)
        .map(|index| {
            let mut row = template.clone();
            row.performance.course = Some(format!("course-{index:05}"));
            row.performance.result_id = Some(format!("result-{index}"));
            row
        })
        .collect())
}

#[test]
fn given_5455_and_5456_keys_when_exporting_then_main_is_bounded_and_overflow_is_lossless()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut previous_headers = None;
    for count in [5_455, 5_456] {
        let path = directory.path().join(format!("columns-{count}.xlsx"));
        write_spreadsheets_with_issues(
            &path,
            &[record(course_observations(count)?)],
            &[],
            &options(),
        )?;
        let mut workbook = open_workbook_auto(&path)?;
        let main = workbook.worksheet_range("Athletes")?;
        assert_eq!(main.height(), 2);
        assert_eq!(main.width(), 16_382);
        let headers = main.rows().next().context("headers")?.to_vec();
        if let Some(previous) = previous_headers {
            assert_eq!(headers, previous);
        }
        previous_headers = Some(headers);
        let detail = workbook.worksheet_range("Observations")?;
        assert_eq!(detail.height(), count + 1);
        let overflow = workbook.worksheet_range("PR_overflow")?;
        assert_eq!(overflow.height(), if count == 5_455 { 1 } else { 2 });
        if count == 5_456 {
            let row = fields(&overflow)?;
            assert_eq!(row["athlete_id"], "42");
            assert!(row["comparison_context"].ends_with("course=course-05455"));
            assert_eq!(row["raw_mark"], "14:49.06");
        }
        assert!(
            !workbook
                .sheet_names()
                .iter()
                .any(|name| name == "Athletes_2")
        );
    }
    Ok(())
}

#[test]
fn given_many_rejected_results_and_long_unicode_fields_when_exporting_then_all_text_reassembles()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("rejected.xlsx");
    let mut template = real_performances()?
        .into_iter()
        .next()
        .context("performance")?;
    template.performance.source_values = Some(serde_json::json!({"level":"hs"}));
    template.performance.reported_status = Some("DQ".to_owned());
    let observations = (0..500)
        .map(|index| {
            let mut row = template.clone();
            row.performance.mark = format!("rejected-mark-{index}");
            row.performance.result_id = Some(format!("rejected-{index}"));
            row
        })
        .collect();
    let mut athlete = record(observations);
    let original_name = format!("=HYPERLINK(\"evil\"){}", "😀".repeat(20_000));
    athlete.name = original_name.clone();
    let issue_text = format!("{}tail", "😀".repeat(40_000));
    let issues = [ExportIssue {
        source: "coverage".to_owned(),
        url: "https://example.test/coverage".to_owned(),
        detail: issue_text.clone(),
    }];
    write_spreadsheets_with_issues(&path, &[athlete], &issues, &options())?;
    let mut workbook = open_workbook_auto(&path)?;
    let main = fields(&workbook.worksheet_range("Athletes")?)?;
    assert!(main["review_notes"].encode_utf16().count() <= 1_024);
    assert!(main["review_notes"].contains("Issues"));
    let rejected = workbook.worksheet_range("Issues")?;
    let issue_column = column(&rejected, "issues")?;
    for index in 0..500 {
        assert!(rejected.rows().skip(1).any(|row| {
            row[issue_column]
                .to_string()
                .contains(&format!("rejected-mark-{index}\""))
        }));
    }
    let details = workbook.worksheet_range("Cell_details")?;
    let sheet = column(&details, "worksheet")?;
    let row_number = column(&details, "row")?;
    let field = column(&details, "field")?;
    let text = column(&details, "text")?;
    let restored_name = details
        .rows()
        .skip(1)
        .filter(|row| {
            row[sheet].to_string() == "Athletes"
                && row[row_number].to_string() == "2"
                && row[field].to_string() == "name"
        })
        .map(|row| row[text].to_string())
        .collect::<String>();
    assert_eq!(restored_name, original_name);
    let issue_id_column = column(&rejected, "issue_id")?;
    let issue_id = rejected
        .rows()
        .skip(1)
        .find(|row| row[1].to_string() == "coverage")
        .context("coverage issue")?[issue_id_column]
        .to_string();
    let parts = rejected
        .rows()
        .skip(1)
        .filter(|row| row[issue_id_column].to_string() == issue_id)
        .collect::<Vec<_>>();
    assert!(parts.len() > 1);
    let part_column = column(&rejected, "part")?;
    let total_column = column(&rejected, "parts")?;
    for (index, row) in parts.iter().enumerate() {
        assert_eq!(row[part_column].to_string(), (index + 1).to_string());
        assert_eq!(row[total_column].to_string(), parts.len().to_string());
    }
    let restored_issue = parts
        .iter()
        .map(|row| row[issue_column].to_string())
        .collect::<String>();
    assert_eq!(restored_issue, issue_text);
    for name in workbook.sheet_names().to_vec() {
        let range = workbook.worksheet_range(&name)?;
        assert!(
            range
                .rows()
                .flatten()
                .all(|cell| cell.to_string().encode_utf16().count() <= 32_767)
        );
        assert!(
            workbook
                .worksheet_formula(&name)?
                .rows()
                .flatten()
                .all(String::is_empty)
        );
    }
    Ok(())
}

#[test]
fn given_incremental_parts_when_finished_then_manifest_names_only_verified_workbooks_with_hashes()
-> Result<()> {
    use sha2::{Digest, Sha256};
    use track_scrapper_v2::export::StagedExport;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("snapshot.xlsx");
    let mut staged = StagedExport::new(&path, &options())?;
    let mut first = record(real_performances()?);
    staged.stage_part(&[first.clone()], &[])?;
    first.id = 43;
    staged.stage_part(&[first], &[])?;
    assert!(!directory.path().join("snapshot.manifest.json").exists());
    assert!(!directory.path().join("snapshot-0001.xlsx").exists());
    let files = staged.finish()?;
    assert_eq!(files.len(), 2);
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
        directory.path().join("snapshot.manifest.json"),
    )?)?;
    assert_eq!(manifest["complete"], true);
    let parts = manifest["parts"].as_array().context("manifest parts")?;
    assert_eq!(parts.len(), files.len());
    let mut identities = Vec::new();
    for (part, file) in parts.iter().zip(&files) {
        let bytes = std::fs::read(file)?;
        assert_eq!(
            part["file"],
            file.file_name()
                .context("file name")?
                .to_str()
                .context("UTF-8 file name")?
        );
        assert_eq!(part["bytes"], u64::try_from(bytes.len())?);
        assert_eq!(part["sha256"], format!("{:x}", Sha256::digest(&bytes)));
        let mut workbook = open_workbook_auto(file)?;
        identities.push(fields(&workbook.worksheet_range("Athletes")?)?["athlete_id"].clone());
    }
    assert_eq!(identities, ["42", "43"]);
    Ok(())
}

#[test]
fn given_destination_race_or_abandoned_stage_when_exporting_then_no_incomplete_manifest_is_published()
-> Result<()> {
    use track_scrapper_v2::export::StagedExport;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("snapshot.xlsx");
    {
        let mut staged = StagedExport::new(&path, &options())?;
        staged.stage_part(&[record(real_performances()?)], &[])?;
    }
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    let mut staged = StagedExport::new(&path, &options())?;
    staged.stage_part(&[record(real_performances()?)], &[])?;
    staged.stage_part(
        &[],
        &[ExportIssue {
            source: "coverage".to_owned(),
            url: "https://example.test/issues".to_owned(),
            detail: "pending".to_owned(),
        }],
    )?;
    let occupied = directory.path().join("snapshot-0002.xlsx");
    std::fs::write(&occupied, b"other owner")?;
    assert!(staged.finish().is_err());
    assert_eq!(std::fs::read(&occupied)?, b"other owner");
    assert!(!directory.path().join("snapshot-0001.xlsx").exists());
    assert!(!directory.path().join("snapshot.manifest.json").exists());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[test]
fn given_empty_incremental_snapshot_when_finished_then_completed_empty_workbook_is_readable()
-> Result<()> {
    use track_scrapper_v2::export::StagedExport;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("empty.xlsx");
    let files = StagedExport::new(&path, &options())?.finish()?;
    assert_eq!(files, [directory.path().join("empty.xlsx")]);
    let mut workbook = open_workbook_auto(&files[0])?;
    assert_eq!(workbook.worksheet_range("Athletes")?.height(), 1);
    assert_eq!(workbook.worksheet_range("Observations")?.height(), 1);
    assert!(directory.path().join("empty.manifest.json").exists());
    Ok(())
}

#[test]
fn given_secret_shaped_source_metadata_when_exporting_then_public_rows_keep_marks_without_credentials()
-> Result<()> {
    use track_scrapper_v2::model::AthleteProfileDetails;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("public.xlsx");
    let mut observation = real_performances()?
        .into_iter()
        .next()
        .context("performance")?;
    let mark = observation.performance.mark.clone();
    observation.performance.source_values = Some(
        serde_json::json!({"level":"ms", "authorization":"private-observation-token", "row":{"accessToken":"private-row-token"}}),
    );
    let mut athlete = record(vec![observation]);
    athlete.profile_details.push(AthleteProfileDetails { source: Source::MileSplit, source_id: "15005189".to_owned(), url: athlete.profiles.first().cloned(), source_values: serde_json::json!({"biography":"Published runner", "token":"private-profile-token"}) });
    write_spreadsheets_with_issues(&path, &[athlete], &[], &options())?;
    let mut workbook = open_workbook_auto(&path)?;
    let observations = workbook.worksheet_range("Observations")?;
    assert_eq!(fields(&observations)?["raw_mark"], mark);
    let mut output = String::new();
    for name in workbook.sheet_names().to_vec() {
        output.push_str(
            &workbook
                .worksheet_range(&name)?
                .rows()
                .flatten()
                .map(ToString::to_string)
                .collect::<String>(),
        );
    }
    assert!(output.contains("Published runner"));
    assert!(output.contains("REDACTED"));
    for secret in [
        "private-observation-token",
        "private-row-token",
        "private-profile-token",
    ] {
        assert!(!output.contains(secret));
    }
    Ok(())
}

#[test]
fn given_overlong_comparison_key_when_exporting_then_key_moves_losslessly_to_supporting_cells()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("long-key.xlsx");
    let mut observations = course_observations(1)?;
    let course = "😀".repeat(20_000);
    observations[0].performance.course = Some(course.clone());
    write_spreadsheets_with_issues(&path, &[record(observations)], &[], &options())?;
    let mut workbook = open_workbook_auto(&path)?;
    let main = workbook.worksheet_range("Athletes")?;
    assert_eq!(main.height(), 2);
    assert!(
        !fields(&main)?
            .keys()
            .any(|header| header.starts_with("best_observed::"))
    );
    let overflow = fields(&workbook.worksheet_range("PR_overflow")?)?;
    assert_eq!(overflow["raw_mark"], "14:49.06");
    assert!(overflow["comparison_context"].contains("Cell_details"));
    let details = workbook.worksheet_range("Cell_details")?;
    let sheet = column(&details, "worksheet")?;
    let field = column(&details, "field")?;
    let text = column(&details, "text")?;
    let key = details
        .rows()
        .skip(1)
        .filter(|row| {
            row[sheet].to_string() == "PR_overflow"
                && row[field].to_string() == "comparison_context"
        })
        .map(|row| row[text].to_string())
        .collect::<String>();
    assert!(key.ends_with(&format!("course={course}")));
    assert!(
        details
            .rows()
            .flatten()
            .all(|cell| cell.to_string().encode_utf16().count() <= 32_767)
    );
    Ok(())
}

#[test]
fn given_one_staged_part_and_unrelated_numbered_neighbor_when_finished_then_requested_name_is_preserved()
-> Result<()> {
    use track_scrapper_v2::export::StagedExport;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("single.xlsx");
    let neighbor = directory.path().join("single-0001.xlsx");
    std::fs::write(&neighbor, b"unrelated existing file")?;
    let mut staged = StagedExport::new(&path, &options())?;
    staged.stage_part(&[record(real_performances()?)], &[])?;
    assert_eq!(staged.finish()?, [path.clone()]);
    let mut workbook = open_workbook_auto(&path)?;
    assert_eq!(
        fields(&workbook.worksheet_range("Athletes")?)?["name"],
        "Brice Fuller"
    );
    assert_eq!(std::fs::read(&neighbor)?, b"unrelated existing file");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
        directory.path().join("single.manifest.json"),
    )?)?;
    assert_eq!(manifest["parts"][0]["file"], "single.xlsx");
    Ok(())
}
