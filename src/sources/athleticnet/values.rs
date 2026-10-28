use anyhow::{Context as _, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::{Athlete, Performance, Sport, UnassignedResult};

pub(super) fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(scalar)
}

pub(super) fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) fn id(value: &Value, key: &str) -> Option<String> {
    text(value, key).filter(|s| s.parse::<u64>().is_ok_and(|n| n > 0))
}

pub(super) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("Athletic.net schema mismatch: expected array {key}"))
}

pub(super) fn object<'a>(
    value: &'a Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, Value>> {
    value
        .get(key)
        .and_then(Value::as_object)
        .with_context(|| format!("Athletic.net schema mismatch: expected object {key}"))
}

pub(super) fn check_identity(expected: Option<&str>, actual: &str) -> Result<()> {
    ensure!(
        expected.is_none_or(|wanted| wanted == actual),
        "Athletic.net identity mismatch: requested {expected:?}, returned {actual}"
    );
    Ok(())
}

pub(super) fn date(raw: &str) -> Option<String> {
    let day = raw.split('T').next()?;
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(day.to_owned())
}

pub(super) fn gender(raw: Option<&str>) -> Option<String> {
    match raw?.trim().to_ascii_uppercase().as_str() {
        "M" | "B" => Some("male".to_owned()),
        "F" | "G" => Some("female".to_owned()),
        _ => None,
    }
}

pub(super) fn name(row: &Value) -> Option<String> {
    let first = row
        .get("FirstName")
        .and_then(Value::as_str)
        .map_or("", str::trim);
    let last = row
        .get("LastName")
        .and_then(Value::as_str)
        .map_or("", str::trim);
    let name = format!("{first} {last}").trim().to_owned();
    (!name.is_empty() && !name.contains(['<', '*', '•'])).then_some(name)
}

pub(super) fn grade(row: &Value, key: &str) -> Option<String> {
    text(row, key).filter(|s| s.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)))
}

pub(super) fn athlete(
    row: &Value,
    identity: String,
    school: Option<String>,
    state: Option<String>,
    sport: Sport,
) -> Option<Athlete> {
    let profile = if sport == Sport::CrossCountry {
        "cross-country"
    } else {
        "track-and-field"
    };
    Some(Athlete {
        profile_url: Some(format!(
            "https://www.athletic.net/athlete/{identity}/{profile}"
        )),
        name: name(row)?,
        id: identity,
        school_id: school,
        state,
        gender: gender(row.get("Gender").and_then(Value::as_str)),
        grade: grade(row, "Grade"),
        ..Athlete::default()
    })
}

pub(super) fn source_row(row: &Value, coordinate: &str) -> String {
    text(row, "shortCode")
        .or_else(|| text(row, "ShortCode"))
        .map_or_else(
            || {
                format!(
                    "{coordinate}:content:{:x}",
                    Sha256::digest(row.to_string().as_bytes())
                )
            },
            |id| format!("shortCode:{id}"),
        )
}

pub(super) fn performance(
    row: &Value,
    athlete_id: String,
    event: String,
    sport: Sport,
) -> Option<Performance> {
    let mark = row.get("Result")?.as_str()?;
    if mark.trim().is_empty() || mark.contains('*') || mark.contains('•') {
        return None;
    }
    Some(Performance {
        athlete_id,
        event,
        mark: mark.to_owned(),
        reported_status: None,
        source_values: None,
        sport,
        result_id: id(row, "IDResult"),
        source_row: None,
        school_id: id(row, "SchoolID").or_else(|| id(row, "TeamID")),
        meet_id: id(row, "MeetID"),
        meet_name: None,
        date: row.get("ResultDate").and_then(Value::as_str).and_then(date),
        wind: row.get("Wind").and_then(Value::as_f64),
        timing: timing(row),
        course: None,
        is_personal_record: ["PersonalBest", "pr"].iter().any(|key| flag(row.get(key))),
        is_relay: false,
        season: row
            .get("SeasonID")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok()),
        grade: grade(row, "Grade"),
        gender: gender(row.get("Gender").and_then(Value::as_str)),
    })
}

pub(super) fn unassigned(
    row: &Value,
    event: String,
    sport: Sport,
    is_relay: bool,
) -> UnassignedResult {
    UnassignedResult {
        result_id: id(row, "IDResult"),
        source_row: None,
        event,
        mark: row
            .get("Result")
            .and_then(Value::as_str)
            .map_or_else(String::new, str::to_owned),
        sport,
        school_id: id(row, "SchoolID").or_else(|| id(row, "TeamID")),
        meet_id: id(row, "MeetID"),
        date: row.get("ResultDate").and_then(Value::as_str).and_then(date),
        is_relay,
        members: Vec::new(),
        raw: row.clone(),
    }
}

pub(super) fn flag(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.as_bool() == Some(true) || v.as_i64().is_some_and(|n| n > 0))
}

fn timing(row: &Value) -> Option<String> {
    if row.get("FAT").and_then(Value::as_i64) == Some(1)
        || row
            .get("Result")
            .and_then(Value::as_str)
            .is_some_and(|s| s.trim().ends_with(['a', 'A']))
    {
        Some("FAT".to_owned())
    } else if row.get("FAT").and_then(Value::as_i64) == Some(0) {
        Some("hand".to_owned())
    } else {
        None
    }
}
