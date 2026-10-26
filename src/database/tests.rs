use anyhow::Result;

use crate::model::{PageContext, PageKind, Performance, Source, Sport, Target};

use super::{Database, keys};

fn target() -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Athlete,
        "https://www.athletic.net/athlete/123".to_owned(),
        PageContext::default(),
    )
}

fn performance() -> Performance {
    Performance {
        athlete_id: "123".to_owned(),
        event: "100m".to_owned(),
        mark: "11.20".to_owned(),
        sport: Sport::Outdoor,
        result_id: None,
        source_row: None,
        school_id: None,
        meet_id: Some("meet-1".to_owned()),
        meet_name: None,
        date: Some("2026-05-01".to_owned()),
        wind: Some(1.2),
        timing: Some("FAT".to_owned()),
        course: None,
        is_personal_record: false,
        is_relay: false,
        season: Some(2026),
        grade: None,
        gender: None,
    }
}

#[test]
fn given_equivalent_get_requests_when_keyed_then_default_and_explicit_methods_match() -> Result<()>
{
    let original = target();
    let mut explicit = original.clone();
    explicit.method = Some("get".to_owned());
    let key = keys::target(&original)?;
    assert_eq!(key, keys::target(&explicit)?);
    assert_eq!(key.len(), 64);
    Ok(())
}

#[test]
fn given_same_url_with_distinct_context_when_keyed_then_each_discovery_is_preserved() -> Result<()>
{
    let original = target();
    let mut context = original.clone();
    context.context.athlete_id = Some("456".to_owned());
    let mut body = original.clone();
    body.body = Some(serde_json::json!({"athleteId": "123"}));
    let key = keys::target(&original)?;
    assert_ne!(key, keys::target(&context)?);
    assert_ne!(key, keys::target(&body)?);
    Ok(())
}

#[test]
fn given_a_source_result_id_when_corrected_then_key_is_independent_of_mark_and_page_position()
-> Result<()> {
    let mut original = performance();
    original.result_id = Some("result-9".to_owned());
    let mut correction = original.clone();
    correction.mark = "11.10".to_owned();
    let key = keys::performance(Source::AthleticNet, &original, "page-a", 0)?;
    assert_eq!(
        key,
        keys::performance(Source::AthleticNet, &correction, "page-b", 7)?
    );
    assert_ne!(
        key,
        keys::performance(Source::MileSplit, &correction, "page-a", 0)?
    );
    Ok(())
}

#[test]
fn given_equal_marks_without_result_ids_when_rows_differ_then_all_rows_are_retained() -> Result<()>
{
    let original = performance();
    let mut correction = original.clone();
    correction.mark = "11.10".to_owned();
    let key = keys::performance(Source::AthleticNet, &original, "page-a", 0)?;
    assert_ne!(
        key,
        keys::performance(Source::AthleticNet, &original, "page-a", 1)?
    );
    assert_ne!(
        key,
        keys::performance(Source::AthleticNet, &correction, "page-a", 0)?
    );
    assert_ne!(
        key,
        keys::performance(Source::AthleticNet, &original, "page-b", 0)?
    );
    Ok(())
}

#[test]
fn source_row_identity_survives_filtered_position_and_mark_changes() -> Result<()> {
    let mut original = performance();
    original.source_row = Some("resultsTF:8".to_owned());
    let mut updated = original.clone();
    updated.mark = "10.99".to_owned();
    updated.event = "corrected event".to_owned();
    let key = keys::performance(Source::AthleticNet, &original, "page-a", 2)?;
    assert_eq!(
        key,
        keys::performance(Source::AthleticNet, &updated, "page-a", 9)?
    );
    updated.source_row = Some("resultsTF:9".to_owned());
    assert_ne!(
        key,
        keys::performance(Source::AthleticNet, &updated, "page-a", 2)?
    );
    Ok(())
}

#[test]
fn given_profile_links_when_identified_then_only_real_provider_athlete_ids_are_accepted()
-> Result<()> {
    let modern = "https://www.athletic.net/athlete/123/track-and-field/high-school";
    let legacy = "https://www.athletic.net/TrackAndField/Athlete.aspx?AID=123";
    let state = "https://tx.milesplit.com/athletes/456/runner";
    let athletic = keys::profile_link(modern)?;
    assert_eq!(athletic, (Source::AthleticNet, "123".to_owned()));
    assert_eq!(athletic, keys::profile_link(legacy)?);
    assert_eq!(
        keys::profile_link(state)?,
        (Source::MileSplit, "456".to_owned())
    );
    assert!(keys::profile_link("https://evilathletic.net/athlete/123").is_err());
    assert!(keys::profile_link("https://milesplit.com.evil.invalid/athletes/456").is_err());
    assert!(keys::profile_link("https://www.athletic.net/team/123").is_err());
    Ok(())
}

#[tokio::test]
async fn given_a_non_postgresql_url_when_connecting_then_it_is_rejected_without_a_fallback() {
    let url = "sqlite://must-not-be-created.sqlite3";
    let error = match Database::connect(url).await {
        Ok(_) => panic!("non-PostgreSQL URL was accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("postgres:// or postgresql://"));
}

#[test]
fn given_a_valid_mark_when_normalized_for_storage_then_units_are_exact_integers() {
    let mark = "11.20";
    let (value, unit, specification) = super::results::normalization("100m", mark, false);
    assert_eq!(value, Some(11_200_000_000));
    assert_eq!(unit, Some("ns"));
    assert_eq!(specification, None);
}

#[test]
fn given_a_failure_or_relay_when_normalized_then_no_individual_numeric_mark_is_invented() {
    let failure = super::results::normalization("100m", "DNF", false);
    let relay = super::results::normalization("100m", "11.20", true);
    let status = super::results::status("DNF", failure.0.is_some());
    assert_eq!(status, "DNF");
    assert_eq!(failure.0, None);
    assert_eq!(failure.1, None);
    assert_eq!(relay.0, None);
    assert_eq!(relay.1, None);
}
