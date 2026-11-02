use serde_json::json;

use super::*;
use crate::model::Sport;

fn performance() -> Performance {
    Performance {
        athlete_id: "athlete-1".to_owned(),
        event: "100m".to_owned(),
        mark: "11.20".to_owned(),
        reported_status: None,
        source_values: None,
        sport: Sport::Outdoor,
        result_id: Some("result-1".to_owned()),
        source_row: None,
        school_id: None,
        meet_id: None,
        meet_name: None,
        date: None,
        wind: Some(1.2),
        timing: None,
        course: None,
        is_personal_record: false,
        is_relay: false,
        season: None,
        grade: None,
        gender: None,
    }
}

#[test]
fn conflict_clause_is_derived_and_returns_stable_source_keys() {
    let (prefix, conflict) =
        batch_sql().expect("the checked-in upsert SQL has the expected structure");
    assert!(prefix.ends_with('\n'));
    assert!(conflict.starts_with("ON CONFLICT (source_result_key) DO UPDATE SET"));
    assert!(!conflict.contains("$26"));
    assert!(!conflict.contains("$27"));
    assert!(conflict.contains("WHEN FALSE THEN NULL"));
    assert!(conflict.contains("THEN NULL::jsonb"));
    assert!(conflict.ends_with("RETURNING source_result_key\n"));
}

#[test]
fn school_replay_skip_is_conservative_for_not_published_attribution() {
    let mut row = performance();
    row.source_values = Some(json!({"school_attribution": "not_published"}));
    assert!(school_replay_candidate(Source::MileSplit, &row));
    assert!(!school_replay_candidate(Source::AthleticNet, &row));
    row.school_id = Some("school-1".to_owned());
    assert!(!school_replay_candidate(Source::MileSplit, &row));
}

#[test]
fn writer_byte_accounting_is_bounded_and_checks_overflow() {
    assert_eq!(checked_payload_bytes(20, 30), Some(50));
    assert_eq!(checked_payload_bytes(MAX_BYTES - 1, 1), Some(MAX_BYTES));
    assert_eq!(checked_payload_bytes(MAX_BYTES, 1), None);
    assert_eq!(checked_payload_bytes(usize::MAX, 1), None);
}

#[test]
fn row_payload_counts_raw_json_and_rejects_size_overflow() {
    let row = performance();
    let normalized = normalized(&row);
    let raw_bytes = serde_json::to_vec(&row)
        .expect("performance serializes to raw JSON")
        .len();
    let bytes = bind_payload_bytes(
        Source::AthleticNet,
        &row,
        "key",
        "https://example.test",
        &normalized,
    );
    assert!(bytes.is_some_and(|bytes| bytes > 0));
    assert!(serialize_raw(&row, raw_bytes).is_some());
    assert!(serialize_raw(&row, raw_bytes.saturating_sub(1)).is_none());
    let large = "x".repeat(MAX_BYTES);
    let mut large_row = row;
    large_row.source_values = Some(json!({"padding": large}));
    assert!(serialize_raw(&large_row, MAX_BYTES).is_none());
}

#[test]
fn bounded_preparation_preserves_exact_canonical_json_numbers_and_nulls() {
    for value in [
        f64::from_bits(1),
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::EPSILON,
        1.234_567_890_123_456_7,
        1e-200,
        -0.0,
    ] {
        let mut row = performance();
        row.source_values = Some(
            json!({"float":value,"unsigned":u64::MAX,"signed":i64::MIN,"nested":[null,"",{"escape":"\n\"\\é"}]}),
        );
        let expected = serde_json::to_value(&row).expect("canonical JSON");
        let (actual, size) = serialize_raw(&row, MAX_BYTES).expect("bounded JSON");
        assert_eq!(actual, expected);
        assert_eq!(
            size,
            serde_json::to_vec(&row).expect("serialized JSON").len()
        );
    }
}
