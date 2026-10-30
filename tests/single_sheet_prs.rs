#[path = "support/records.rs"]
mod records;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    export::{ExportLayout, ExportOptions, StagedExport},
    model::{
        AthleteProfileDetails, AthleteRecord, Coach, EvidencedPerformance, School, Source, Sport,
    },
};

fn athlete() -> AthleteRecord {
    AthleteRecord {
        id: 11,
        name: "Public Athlete".into(),
        gender: Some("female".into()),
        grade: None,
        graduation_year: Some(2027),
        needs_review: false,
        schools: vec![School {
            id: "published-wi-school".into(),
            name: "Published WI School".into(),
            state: Some("WI".into()),
            ..Default::default()
        }],
        coaches: Vec::new(),
        profiles: Vec::new(),
        performances: Vec::new(),
        profile_details: Vec::new(),
    }
}

fn observation(mark: &str, level: &str) -> EvidencedPerformance {
    let mut row = records::performance(mark);
    row.is_personal_record = false;
    row.source_values = Some(serde_json::json!({"level":level}));
    EvidencedPerformance {
        source: Source::MileSplit,
        url: "https://wi.milesplit.com/meets/123/results/1".into(),
        performance: row,
    }
}

fn export(record: &AthleteRecord) -> Result<BTreeMap<String, String>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("single.xlsx");
    let options = ExportOptions {
        states: vec!["WI".into()],
        graduation_years: vec![2027],
        rows_per_file: 1000,
        per_athlete_sheets: true,
    };
    let mut export = StagedExport::new(&path, &options)?.with_layout(ExportLayout::SingleSheet);
    export.stage_part(std::slice::from_ref(record), &[])?;
    export.finish()?;
    let mut workbook = open_workbook_auto(&path)?;
    assert_eq!(workbook.sheet_names(), &["Athletes"]);
    let sheet = workbook.worksheet_range("Athletes")?;
    assert_eq!(sheet.height(), 2);
    Ok(sheet
        .rows()
        .next()
        .context("headers")?
        .iter()
        .zip(sheet.rows().nth(1).context("athlete")?)
        .map(|(header, value)| (header.to_string(), value.to_string()))
        .collect())
}

#[test]
fn ordinary_results_supply_context_and_level_specific_bests_deterministically() -> Result<()> {
    let mut athlete = athlete();
    athlete.performances = vec![
        observation("11.00", "hs"),
        observation("10.80", "hs"),
        observation("9.80", "ms"),
    ];
    let mut assisted = observation("10.50", "hs");
    assisted.performance.wind = Some(3.0);
    let mut hand = observation("10.60", "hs");
    hand.performance.timing = Some("HT".into());
    let mut stale = observation("9.00", "hs");
    stale.performance.source_values =
        Some(serde_json::json!({"level":"hs","revision_status":"unverified_prior_revision"}));
    athlete.performances.extend([assisted, hand, stale]);
    let first = export(&athlete)?;
    let best = &first["Outdoor 100m best [hs; milesplit]"];
    assert!(best.contains("10.80 (2026-05-15; FAT; wind=0.8)"));
    assert!(best.contains("10.50 (2026-05-15; FAT; wind=3)"));
    assert!(best.contains("10.60 (2026-05-15; HT; wind=0.8)"));
    assert!(!best.contains("11.00") && !best.contains("9.00"));
    assert!(first["Outdoor 100m best [ms; milesplit]"].starts_with("9.80"));
    athlete.performances.reverse();
    assert_eq!(export(&athlete)?, first);
    Ok(())
}

#[test]
fn headline_only_pr_is_visible_without_guessing_sport_or_exposing_masked_marks() -> Result<()> {
    let mut athlete = athlete();
    athlete.profile_details.push(AthleteProfileDetails {
        source: Source::MileSplit,
        source_id: "11".into(),
        url: Some("https://wi.milesplit.com/athletes/11-athlete".into()),
        source_values: serde_json::json!({"pages":{"https://wi.milesplit.com/athletes/11-athlete":{"pr_rows":[
            {"masked":false,"html":"<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>800m</td><td class='pr-td-mark'>2:10.00</td><td class='pr-td-date'>Apr 14, 2026</td></tr>"},
            {"masked":true,"html":"<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>1600m</td><td class='pr-td-mark'>4:10.00</td></tr>"},
            {"masked":false,"html":"<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>3200m</td><td class='pr-td-mark mask'>8:10.00</td></tr>"}
        ]}}}),
    });
    let fields = export(&athlete)?;
    assert_eq!(
        fields["All seasons 800m PR [all levels; milesplit]"],
        "2:10.00 (2026-04-14)"
    );
    assert!(
        !fields
            .keys()
            .any(|key| key.contains("1600m") || key.contains("3200m"))
    );
    assert!(
        !fields
            .keys()
            .filter(|key| key.contains(" PR [") || key.contains(" best ["))
            .any(|key| key.starts_with("Indoor ")
                || key.starts_with("Outdoor ")
                || key.starts_with("XC "))
    );
    assert!(
        !fields
            .values()
            .any(|value| value.contains("<tr") || value.contains("pr_rows"))
    );
    Ok(())
}

#[test]
fn coach_contacts_keep_their_pairing_and_use_published_programs() -> Result<()> {
    let mut athlete = athlete();
    athlete.coaches = [
        (
            "Alice",
            Some("z@example.edu"),
            "Head Coach — Boys Track and Field",
            None,
        ),
        ("Bob", Some("a@example.edu"), "Assistant Track Coach", None),
        ("Casey", None, "Track Coach", Some(Sport::Outdoor)),
        (
            "Dana",
            Some("d@example.edu"),
            "Head Cross Country Coach",
            None,
        ),
        ("Evan", Some("e@example.edu"), "Head Coach", None),
    ]
    .into_iter()
    .map(|(name, email, role, sport)| Coach {
        school_id: "school".into(),
        name: Some(name.into()),
        email: email.map(str::to_owned),
        role: role.into(),
        sport,
        review_notes: Vec::new(),
    })
    .collect();
    let first = export(&athlete)?;
    assert_eq!(first["Track coach"], "Alice; Bob; Casey");
    assert_eq!(
        first["Track coach email"],
        "z@example.edu; a@example.edu; —"
    );
    assert_eq!(first["XC coach"], "Dana");
    assert_eq!(first["XC coach email"], "d@example.edu");
    assert_eq!(first["Coach"], "Evan");
    assert_eq!(first["Coach email"], "e@example.edu");
    athlete.coaches.reverse();
    assert_eq!(export(&athlete)?, first);
    Ok(())
}

#[test]
fn xc_unknown_courses_and_field_specifications_do_not_compete() -> Result<()> {
    let mut athlete = athlete();
    athlete.performances = [
        ("5k", "16:10", Sport::CrossCountry, "A"),
        ("5k", "17:10", Sport::CrossCountry, "B"),
        ("Shot Put (4kg)", "12.0m", Sport::Outdoor, "C"),
        ("Shot Put (6kg)", "11.0m", Sport::Outdoor, "D"),
    ]
    .into_iter()
    .map(|(event, mark, sport, meet)| {
        let mut evidence = observation(mark, "hs");
        evidence.performance.event = event.into();
        evidence.performance.sport = sport;
        evidence.performance.result_id = Some(meet.into());
        evidence.performance.meet_name = Some(meet.into());
        evidence.performance.timing = None;
        evidence.performance.wind = None;
        evidence
    })
    .collect();
    let first = export(&athlete)?;
    assert!(first["XC 5000m best [hs; milesplit]"].contains("16:10"));
    assert!(first["XC 5000m best [hs; milesplit]"].contains("17:10"));
    assert!(first["Outdoor shot_put best [hs; milesplit]"].contains("12.0m"));
    assert!(first["Outdoor shot_put best [hs; milesplit]"].contains("11.0m"));
    assert!(first["Outdoor shot_put best [hs; milesplit]"].contains("spec=Shot Put (4kg)"));
    assert!(first["Outdoor shot_put best [hs; milesplit]"].contains("spec=Shot Put (6kg)"));
    athlete.performances.reverse();
    assert_eq!(export(&athlete)?, first);
    Ok(())
}
