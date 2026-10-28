mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    database::Database,
    export::{ExportOptions, write_spreadsheets_with_issues},
    model::{Capture, PageKind, ParsedPage, Sport, Target},
    sources::milesplit::{athlete_target, meet_target, parse},
};

const RAW: &str = include_str!("../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html");
const PROFILE: &str = include_str!("../fixtures/milesplit/athlete-oh-15005189.html");
const AR: &str = include_str!("../fixtures/milesplit/meet-ar-770517-results.html");
const GIRLS: &str = include_str!("../fixtures/milesplit/meet-al-754110-results.html");

struct PublishedFixture {
    meet: &'static str,
    site: &'static str,
    file: &'static str,
    body: &'static str,
    sport: Sport,
    rows: usize,
    first_mark: &'static str,
}

fn fixtures() -> [PublishedFixture; 4] {
    [
        PublishedFixture {
            meet: "770621",
            site: "oh",
            file: "1321880",
            body: RAW,
            sport: Sport::CrossCountry,
            rows: 80,
            first_mark: "12:40.6",
        },
        PublishedFixture {
            meet: "771781",
            site: "al",
            file: "1327928",
            body: include_str!("../fixtures/milesplit/meet-al-771781-results.html"),
            sport: Sport::CrossCountry,
            rows: 73,
            first_mark: "20:51.3",
        },
        PublishedFixture {
            meet: "770517",
            site: "ar",
            file: "1311117",
            body: AR,
            sport: Sport::Outdoor,
            rows: 175,
            first_mark: "12.74",
        },
        PublishedFixture {
            meet: "776188",
            site: "al",
            file: "1325876",
            body: include_str!("../fixtures/milesplit/meet-al-776188-results.html"),
            sport: Sport::CrossCountry,
            rows: 37,
            first_mark: "19:13.75",
        },
    ]
}

fn aliases(fixture: &PublishedFixture) -> Result<[Target; 2]> {
    let mut root = meet_target(fixture.meet, fixture.sport)?;
    root.url = format!(
        "https://{}.milesplit.com/meets/{}/results",
        fixture.site, fixture.meet
    );
    let mut file = root.clone();
    file.kind = PageKind::Results;
    file.url = format!("{}/{}/raw", root.url, fixture.file);
    Ok([root, file])
}

fn result_ids(page: &ParsedPage) -> Result<Vec<&str>> {
    page.unassigned_results
        .iter()
        .map(|row| row.result_id.as_deref().context("published row ID"))
        .collect()
}

#[test]
fn given_real_published_files_when_root_and_numbered_aliases_parse_then_all_ids_and_marks_match()
-> Result<()> {
    for fixture in fixtures() {
        let [root, file] = aliases(&fixture)?;
        let root_page = parse(&root, fixture.body)?;
        let file_page = parse(&file, fixture.body)?;
        assert_eq!(root_page.unassigned_results.len(), fixture.rows);
        assert_eq!(result_ids(&root_page)?, result_ids(&file_page)?);
        assert_eq!(root_page.unassigned_results[0].mark, fixture.first_mark);
        assert!(root_page.performances.is_empty() && file_page.performances.is_empty());
        assert!(root_page.athletes.is_empty() && file_page.athletes.is_empty());
        assert!(
            !root_page
                .notes
                .iter()
                .any(|note| note.contains("Unresolved published-file identity"))
        );
    }
    Ok(())
}

async fn replay_alias(
    test: &support::Scenario,
    database: &Database,
    fixture: &PublishedFixture,
) -> Result<()> {
    let [root, file] = aliases(fixture)?;
    for target in [&root, &file, &root] {
        let page = parse(target, fixture.body)?;
        let capture = Capture {
            url: target.url.clone(),
            status: 200,
            content_type: "text/html".into(),
            body: fixture.body.as_bytes().to_vec(),
        };
        database.persist(None, target, &capture, &page).await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM results WHERE source='milesplit' AND meet_id=$1",
        )
        .bind(fixture.meet)
        .fetch_one(&test.pool)
        .await?;
        assert_eq!(count, i64::try_from(fixture.rows)?);
    }
    Ok(())
}

#[tokio::test]
async fn given_real_file_aliases_when_replayed_into_postgres_then_result_counts_never_increase()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for fixture in fixtures() {
                replay_alias(test, &database, &fixture).await?;
            }
            let counts: (i64, i64) =
                sqlx::query_as("SELECT count(*),count(DISTINCT source_result_id) FROM results")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(counts, (365, 365));
            let athletes: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(athletes, 0);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[test]
fn given_another_published_file_or_unproven_root_when_parsed_then_marks_never_deduplicate_without_evidence()
-> Result<()> {
    let mut root = meet_target("754110", Sport::CrossCountry)?;
    root.url = "https://al.milesplit.com/meets/754110/results".into();
    let girls = parse(&root, GIRLS)?;
    let mut other = root.clone();
    other.kind = PageKind::Results;
    other.url = format!("{}/1330427/raw", root.url);
    let conflicting = parse(&other, GIRLS)?;
    assert_eq!(girls.unassigned_results.len(), 102);
    assert_eq!(conflicting.unassigned_results.len(), 102);
    assert_ne!(result_ids(&girls)?, result_ids(&conflicting)?);
    assert_eq!(
        girls.unassigned_results[0].mark,
        conflicting.unassigned_results[0].mark
    );
    assert!(
        conflicting
            .notes
            .iter()
            .any(|note| note.contains("requested and canonical result files conflict"))
    );
    let canonical = "<link rel=\"canonical\" href=\"https://oh.milesplit.com/meets/770621-beaver-eastern-invite-2026/results/1321880/raw\" />";
    assert!(RAW.contains(canonical));
    let without_identity = RAW.replacen(canonical, "", 1);
    let fixture = fixtures().into_iter().next().context("raw fixture")?;
    let [root, numbered] = aliases(&fixture)?;
    let unresolved = parse(&root, &without_identity)?;
    let numbered = parse(&numbered, &without_identity)?;
    assert_ne!(result_ids(&unresolved)?, result_ids(&numbered)?);
    assert_eq!(unresolved.unassigned_results.len(), 80);
    assert!(
        unresolved
            .unassigned_results
            .iter()
            .all(|row| row.raw["published_file_identity"].is_null())
    );
    assert!(
        numbered
            .unassigned_results
            .iter()
            .all(|row| row.raw["kind"] == "raw_result"
                && row.raw["published_file_identity"] == "milesplit-file:770621:1321880")
    );
    assert!(
        unresolved
            .notes
            .iter()
            .any(|note| note.contains("no published numbered file evidence"))
    );
    Ok(())
}

#[test]
fn given_distinct_published_file_coordinates_when_rows_are_identical_then_files_remain_distinct()
-> Result<()> {
    let canonical = "<link rel=\"canonical\" href=\"https://al.milesplit.com/meets/754110-northeast-alabama-championship-2026/results/1330423/raw\" />";
    assert!(GIRLS.contains(canonical));
    let body = GIRLS.replacen(canonical, "", 1).replace(" selected>", ">");
    let mut requested = meet_target("754110", Sport::CrossCountry)?;
    requested.kind = PageKind::Results;
    requested.url = "https://al.milesplit.com/meets/754110/results/1330423/raw".into();
    let first = parse(&requested, &body)?;
    requested.url = "https://al.milesplit.com/meets/754110/results/1330427/raw".into();
    let second = parse(&requested, &body)?;
    assert_eq!(first.unassigned_results.len(), 102);
    assert_eq!(second.unassigned_results.len(), 102);
    assert_ne!(result_ids(&first)?, result_ids(&second)?);
    assert_eq!(
        first
            .unassigned_results
            .iter()
            .map(|row| (&row.event, &row.mark))
            .collect::<Vec<_>>(),
        second
            .unassigned_results
            .iter()
            .map(|row| (&row.event, &row.mark))
            .collect::<Vec<_>>(),
    );
    assert!(
        first
            .notes
            .iter()
            .chain(&second.notes)
            .all(|note| !note.contains("Unresolved published-file identity"))
    );
    Ok(())
}

#[test]
fn given_real_profile_when_mark_cell_itself_or_descendant_is_masked_then_no_mark_is_fabricated()
-> Result<()> {
    let target = athlete_target("15005189", Some("OH"), Sport::Indoor)?;
    let original = "<td class=\"pr-td-mark\">2:10.99</td>";
    assert!(PROFILE.contains(original));
    let baseline = parse(&target, PROFILE)?;
    let masked_id = "pr:15005189:indoor:all:800m";
    assert!(
        baseline
            .performances
            .iter()
            .any(|row| row.result_id.as_deref() == Some(masked_id) && row.mark == "2:10.99")
    );
    for attribute in [
        "class='pr-td-mark mask'",
        "class='pr-td-mark redact'",
        "class='pr-td-mark record--locked'",
        "class='pr-td-mark paywall'",
        "class='pr-td-mark' aria-label='Locked'",
    ] {
        for replacement in [
            format!("<td {attribute}>2:10.99</td>"),
            format!("<td><span {attribute}>2:10.99</span></td>"),
        ] {
            let body = PROFILE.replacen(original, &replacement, 1);
            let page = parse(&target, &body)?;
            assert_eq!(page.performances.len(), baseline.performances.len() - 1);
            assert!(
                !page
                    .performances
                    .iter()
                    .any(|row| row.result_id.as_deref() == Some(masked_id))
            );
            assert!(page.performances.iter().any(|row| row.result_id.as_deref()
                == Some("pr:15005189:indoor:ms:800m")
                && row.mark == "2:10.99"));
            assert!(page.performances.iter().all(|row| {
                baseline
                    .performances
                    .iter()
                    .any(|known| known.result_id == row.result_id && known.mark == row.mark)
            }));
            assert!(
                page.notes
                    .iter()
                    .any(|note| note.contains("masked PR indoor 800m"))
            );
        }
    }
    Ok(())
}

#[test]
fn given_real_meet_with_generic_track_metadata_when_context_is_indoor_then_no_outdoor_is_invented()
-> Result<()> {
    assert!(AR.contains("\"sport\": \"Outdoor Track & Field\""));
    let generic = AR.replacen(
        "\"sport\": \"Outdoor Track & Field\"",
        "\"sport\": \"TrackAndField\"",
        1,
    );
    let mut target = meet_target("770517", Sport::Indoor)?;
    target.url = "https://ar.milesplit.com/meets/770517/results".into();
    let indoor = parse(&target, &generic)?;
    assert_eq!(indoor.unassigned_results.len(), 175);
    assert!(
        indoor
            .unassigned_results
            .iter()
            .all(|row| row.sport == Sport::Indoor)
    );
    assert_eq!(indoor.unassigned_results[0].mark, "12.74");
    let outdoor = parse(&target, AR)?;
    assert!(
        outdoor
            .unassigned_results
            .iter()
            .all(|row| row.sport == Sport::Outdoor)
    );
    target.context.sport = None;
    let error = parse(&target, &generic)
        .err()
        .context("generic sport was guessed")?;
    assert!(error.to_string().contains("unresolved MileSplit sport"));
    Ok(())
}

fn raw_individual_with_synthetic_hs_evidence(target: &Target) -> Result<(String, ParsedPage)> {
    let body = RAW
        .replacen(
            "Jeydyn Fields",
            "<a href='/athletes/123456'>Jeydyn Fields</a>",
            1,
        )
        .replacen("Boys Middle School 3000 Meter", "Boys 3000 Meter", 1);
    let mut page = parse(target, &body)?;
    assert_eq!(page.performances.len(), 1);
    assert_eq!(page.performances[0].event, "Boys 3000 Meter");
    track_scrapper_v2::marks::comparable_performance(
        track_scrapper_v2::model::Source::MileSplit,
        &target.url,
        &page.performances[0],
    )
    .context("synthetic HS raw individual must be a supported comparable event")?;
    let values = page
        .performances
        .first_mut()
        .context("individual result")?
        .source_values
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
        .context("raw result evidence")?;
    assert_eq!(values["kind"], "raw_result");
    assert_eq!(
        values["published_file_identity"],
        "milesplit-file:770621:1321880"
    );
    values.insert("level".to_owned(), serde_json::json!("hs"));
    Ok((body, page))
}

async fn persist_raw_capture(
    database: &Database,
    target: &Target,
    body: &str,
    page: &ParsedPage,
) -> Result<()> {
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".to_owned(),
        body: body.as_bytes().to_vec(),
    };
    database.persist(None, target, &capture, page).await?;
    Ok(())
}

async fn assert_raw_revision_export(
    database: &Database,
    test: &support::Scenario,
    eligible: bool,
) -> Result<()> {
    let (records, issues) = database
        .export_snapshot(&["OH".to_owned()], &[2027])
        .await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].performances.len(), 1);
    let path = test.directory().join(if eligible {
        "before-relay.xlsx"
    } else {
        "after-relay.xlsx"
    });
    write_spreadsheets_with_issues(
        &path,
        &records,
        &issues,
        &ExportOptions {
            states: vec!["OH".to_owned()],
            graduation_years: vec![2027],
            rows_per_file: 100,
            per_athlete_sheets: false,
        },
    )?;
    let mut workbook = open_workbook_auto(path)?;
    let main = workbook.worksheet_range("Athletes")?;
    let headers = main.rows().next().context("athlete headers")?;
    let athlete = main.rows().nth(1).context("retained athlete")?;
    assert_eq!(
        headers
            .iter()
            .zip(athlete)
            .any(|(key, value)| key.to_string().ends_with("::mark")
                && value.to_string() == "12:40.6"),
        eligible
    );
    let observations = workbook.worksheet_range("Observations")?;
    assert_eq!(observations.height(), 2);
    let eligibility = observations
        .rows()
        .next()
        .context("observation headers")?
        .iter()
        .position(|cell| cell.to_string() == "hs_pr_eligibility")
        .context("HS eligibility column")?;
    let row = observations.rows().nth(1).context("retained observation")?;
    let status = row.get(eligibility).context("HS eligibility")?.to_string();
    if eligible {
        assert_eq!(status, "eligible");
    } else {
        assert!(status.contains("unverified prior revision"));
    }
    Ok(())
}

#[tokio::test]
async fn given_raw_individual_when_same_file_changes_to_relay_then_prior_is_retained_flagged_and_cannot_win()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let fixture = fixtures().into_iter().next().context("raw fixture")?;
        let [_, target] = aliases(&fixture)?;
        let (individual_body, individual) = raw_individual_with_synthetic_hs_evidence(&target)?;
        let individual_id = individual.performances[0].result_id.clone().context("individual ID")?;
        persist_raw_capture(&database, &target, &individual_body, &individual).await?;
        assert_raw_revision_export(&database, test, true).await?;
        let relay_body = individual_body.replacen("Boys 3000 Meter", "Boys 4x400 Meter Relay", 1);
        let relay = parse(&target, &relay_body)?;
        assert!(relay.performances.is_empty() && relay.athletes.is_empty());
        let relay_row = relay.unassigned_results.first().context("relay row")?;
        let relay_id = relay_row.result_id.as_deref().context("relay ID")?;
        assert_ne!(relay_id, individual_id);
        assert!(relay_row.is_relay);
        assert_eq!(relay_row.mark, "12:40.6");
        assert_eq!(relay_row.raw["published_file_identity"], "milesplit-file:770621:1321880");
        persist_raw_capture(&database, &target, &relay_body, &relay).await?;
        let prior: (bool, bool, String, String) = sqlx::query_as(
            "SELECT athlete_id IS NOT NULL,is_relay,raw_mark,raw->'source_values'->>'revision_status' FROM results WHERE source_result_id=$1"
        ).bind(&individual_id).fetch_one(&test.pool).await?;
        assert_eq!(prior, (true, false, "12:40.6".to_owned(), "unverified_prior_revision".to_owned()));
        let current: (bool, bool, String) = sqlx::query_as(
            "SELECT athlete_id IS NULL,is_relay,raw_mark FROM results WHERE source_result_id=$1"
        ).bind(relay_id).fetch_one(&test.pool).await?;
        assert_eq!(current, (true, true, "12:40.6".to_owned()));
        let review: bool = sqlx::query_scalar("SELECT needs_review FROM athletes").fetch_one(&test.pool).await?;
        assert!(review);
        assert_raw_revision_export(&database, test, false).await?;
        database.close().await;
        Ok(())
    })).await
}
