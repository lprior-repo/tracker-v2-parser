use anyhow::{Context, Result};
use calamine::{Data, Range, Reader, Xlsx, open_workbook};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufReader;
use track_scrapper_v2::export::{ExportIssue, ExportOptions, write_spreadsheets_with_issues};
use track_scrapper_v2::model::{
    AthleteRecord, EvidencedPerformance, Performance, School, Source, Sport,
};

const LINK_LIMIT: usize = 65_530;
type Workbook = Xlsx<BufReader<File>>;

fn options() -> ExportOptions {
    ExportOptions {
        states: vec!["WI".to_owned()],
        graduation_years: vec![2027],
        rows_per_file: 100_000,
        per_athlete_sheets: false,
    }
}

fn evidence(index: usize, event: &str) -> EvidencedPerformance {
    EvidencedPerformance {
        source: Source::AthleticNet,
        url: format!("https://www.athletic.net/result/{index}"),
        performance: Performance {
            athlete_id: "42".to_owned(),
            event: event.to_owned(),
            mark: "11.20".to_owned(),
            sport: Sport::Outdoor,
            result_id: Some(format!("result-{index}")),
            source_row: None,
            reported_status: None,
            source_values: None,
            school_id: None,
            meet_id: None,
            meet_name: None,
            date: Some("2026-05-15".to_owned()),
            wind: Some(0.0),
            timing: Some("FAT".to_owned()),
            course: None,
            is_personal_record: false,
            is_relay: false,
            season: Some(2026),
            grade: Some("11".to_owned()),
            gender: Some("M".to_owned()),
        },
    }
}

fn athlete(id: i64) -> AthleteRecord {
    AthleteRecord {
        id,
        name: format!("Runner {id}"),
        gender: Some("M".to_owned()),
        grade: Some("11".to_owned()),
        graduation_year: Some(2027),
        needs_review: false,
        schools: vec![School {
            id: "school".to_owned(),
            name: "Wisconsin High".to_owned(),
            state: Some("WI".to_owned()),
            ..School::default()
        }],
        coaches: Vec::new(),
        profiles: vec![format!("https://www.athletic.net/athlete/{id}")],
        performances: vec![evidence(0, "100 Meters"), evidence(1, "200 Meters")],
    }
}

fn column(range: &Range<Data>, header: &str) -> Result<usize> {
    range
        .rows()
        .next()
        .context("missing headers")?
        .iter()
        .position(|value| value.to_string() == header)
        .with_context(|| format!("missing {header} header"))
}

fn verify_clickable_cells(
    workbook: &mut Workbook,
    name: &str,
    range: &Range<Data>,
) -> Result<usize> {
    let links = workbook.hyperlinks_by_sheet_name(name)?;
    assert!(links.len() <= LINK_LIMIT);
    for link in &links {
        assert_eq!(link.range.start, link.range.end);
        let cell = range
            .get_value(link.range.start)
            .context("missing linked cell")?;
        assert_eq!(link.target.as_deref(), Some(cell.to_string().as_str()));
    }
    Ok(links.len())
}

fn partition_names(workbook: &Workbook, base: &str) -> Vec<String> {
    workbook
        .sheet_names()
        .into_iter()
        .filter(|name| name == base || name.starts_with(&format!("{base}_")))
        .collect()
}

#[test]
fn given_over_65535_linked_issues_when_exporting_then_every_identity_and_url_reopens_clickable()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("issues.xlsx");
    let mut record = athlete(42);
    record.needs_review = true;
    let issues: Vec<_> = (0..65_536)
        .map(|index| ExportIssue {
            source: format!("source-{index}"),
            url: format!("https://www.athletic.net/unassigned/{index}"),
            detail: format!("unassigned-result-{index}"),
        })
        .collect();
    assert_eq!(
        write_spreadsheets_with_issues(&path, &[record], &issues, &options())?,
        vec![path.clone()]
    );
    let mut workbook: Workbook = open_workbook(&path)?;
    let names = partition_names(&workbook, "Issues");
    assert_eq!(names, ["Issues", "Issues_2"]);
    let mut next = 0;
    let mut links = 0;
    for (part, name) in names.iter().enumerate() {
        let range = workbook.worksheet_range(name)?;
        assert_eq!(range.width(), 4);
        assert_eq!(range.height(), if part == 0 { LINK_LIMIT + 1 } else { 8 });
        links += verify_clickable_cells(&mut workbook, name, &range)?;
        for row in range.rows().skip(1) {
            if row[0].to_string() == "42" {
                assert_eq!(row[2].to_string(), "https://www.athletic.net/athlete/42");
                assert!(row[3].to_string().contains("unresolved identity"));
            } else {
                let expected = &issues[next];
                assert_eq!(row[0].to_string(), "");
                assert_eq!(row[1].to_string(), expected.source);
                assert_eq!(row[2].to_string(), expected.url);
                assert_eq!(row[3].to_string(), expected.detail);
                next += 1;
            }
        }
    }
    assert_eq!(next, issues.len());
    assert_eq!(links, issues.len() + 1);
    assert_eq!(workbook.worksheet_range("Athletes")?.height(), 1);
    Ok(())
}

#[test]
fn given_many_three_link_athletes_when_exporting_then_partitions_count_links_not_rows() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("athletes.xlsx");
    let records: Vec<_> = (0..21_845).map(athlete).collect();
    write_spreadsheets_with_issues(&path, &records, &[], &options())?;
    let mut workbook: Workbook = open_workbook(&path)?;
    let names = partition_names(&workbook, "Athletes");
    assert_eq!(names, ["Athletes", "Athletes_2"]);
    let mut identities = BTreeSet::new();
    let mut hyperlinks = 0;
    let mut first_headers = None;
    for (part, name) in names.iter().enumerate() {
        let range = workbook.worksheet_range(name)?;
        assert_eq!(range.height(), if part == 0 { 21_844 } else { 3 });
        let headers = range.rows().next().context("missing headers")?.to_vec();
        assert_eq!(headers.len(), 21);
        if let Some(previous) = &first_headers {
            assert_eq!(&headers, previous);
        }
        first_headers = Some(headers);
        hyperlinks += verify_clickable_cells(&mut workbook, name, &range)?;
        let id_column = column(&range, "athlete_id")?;
        let profile_column = column(&range, "source_profiles")?;
        for row in range.rows().skip(1) {
            let id = row[id_column].to_string().parse::<i64>()?;
            assert!(identities.insert(id));
            assert_eq!(
                row[profile_column].to_string(),
                format!("https://www.athletic.net/athlete/{id}")
            );
        }
    }
    assert_eq!(identities, records.iter().map(|record| record.id).collect());
    assert_eq!(hyperlinks, records.len() * 3);
    assert_eq!(
        workbook.worksheet_range("Issues")?.height(),
        records.len() + 1
    );
    Ok(())
}

#[test]
fn given_one_athlete_over_detail_link_capacity_when_exporting_then_all_performances_are_retained()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("detail.xlsx");
    let mut record = athlete(42);
    record.performances = (0..65_536)
        .map(|index| evidence(index, "100 Meters"))
        .collect();
    let mut options = options();
    options.per_athlete_sheets = true;
    write_spreadsheets_with_issues(&path, &[record], &[], &options)?;
    let mut workbook: Workbook = open_workbook(&path)?;
    assert_eq!(
        workbook.sheet_names(),
        ["Athletes", "Issues", "Athlete_42", "Athlete_42_2"]
    );
    let mut next = 0;
    let mut hyperlinks = 0;
    for (part, name) in partition_names(&workbook, "Athlete_42").iter().enumerate() {
        let range = workbook.worksheet_range(name)?;
        assert_eq!(range.width(), 8);
        assert_eq!(range.height(), if part == 0 { LINK_LIMIT + 1 } else { 7 });
        hyperlinks += verify_clickable_cells(&mut workbook, name, &range)?;
        for row in range.rows().skip(1) {
            assert_eq!(row[0].to_string(), "100 Meters");
            assert_eq!(row[1].to_string(), "11.20");
            assert_eq!(
                row[4].to_string(),
                format!("https://www.athletic.net/result/{next}")
            );
            assert_eq!(row[5].to_string(), "2026-05-15");
            assert!(row[7].to_string().contains("100m"));
            next += 1;
        }
    }
    assert_eq!(next, 65_536);
    assert_eq!(hyperlinks, next);
    assert_eq!(workbook.worksheet_range("Athletes")?.height(), 2);
    assert_eq!(workbook.worksheet_range("Issues")?.height(), 2);
    Ok(())
}
