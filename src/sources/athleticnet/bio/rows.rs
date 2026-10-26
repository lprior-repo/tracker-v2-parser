use serde_json::Value;

use crate::model::{Athlete, ParsedPage, Performance, Sport, UnassignedResult};

use super::super::values as v;
use super::{season_number, season_sport};

pub(super) fn retain(
    body: &Value,
    key: &str,
    athlete: &Athlete,
    requested: Sport,
    page: &mut ParsedPage,
) {
    let Some(rows) = body.get(key).and_then(Value::as_array) else {
        page.notes.push(format!(
            "{key} is absent or null, not evidence of an empty result history"
        ));
        return;
    };
    for (ordinal, row) in rows.iter().enumerate() {
        let source_row = v::source_row(row, &format!("{key}:{ordinal}"));
        match project(body, key, row, athlete) {
            Some(performance) if performance.is_relay => {
                let mut result = unassigned(body, key, row, requested);
                result.source_row = Some(source_row);
                result.event = performance.event;
                result.sport = performance.sport;
                result.date = performance.date;
                result.is_relay = true;
                retain_members(body, row, &mut result);
                page.unassigned_results.push(result);
            }
            Some(mut performance) => {
                performance.source_row = Some(source_row);
                if performance.date.is_none()
                    || performance.meet_id.is_none()
                    || performance.school_id.is_none()
                {
                    page.notes.push(format!(
                        "{key} result {:?} has incomplete published date, meet, or school evidence",
                        performance.result_id
                    ));
                }
                page.performances.push(performance);
            }
            None => {
                let mut result = unassigned(body, key, row, requested);
                result.source_row = Some(source_row);
                if result.is_relay {
                    retain_members(body, row, &mut result);
                }
                page.notes.push(format!("{key} result {:?} has masked/incomplete mark, event, or season evidence; raw row retained", result.result_id));
                page.unassigned_results.push(result);
            }
        }
    }
}

pub(super) fn retain_unidentified(body: &Value, requested: Sport, page: &mut ParsedPage) {
    for key in ["resultsTF", "resultsXC"] {
        body.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .for_each(|(ordinal, row)| {
                let mut result = unassigned(body, key, row, requested);
                result.source_row = Some(v::source_row(row, &format!("{key}:{ordinal}")));
                if result.is_relay {
                    retain_members(body, row, &mut result);
                }
                page.unassigned_results.push(result);
            });
    }
}

fn project(body: &Value, key: &str, row: &Value, athlete: &Athlete) -> Option<Performance> {
    let school_id = v::id(row, "SchoolID");
    let season = season_number(row, "SeasonID");
    let xc = key == "resultsXC";
    let sport = if xc {
        Sport::CrossCountry
    } else {
        row_sport(body, row)?
    };
    let event = event_label(body, key, row)?;
    let mut result = v::performance(row, athlete.id.clone(), event, sport)?;
    result.is_relay = is_relay(body, row, &result.event);
    result.gender.clone_from(&athlete.gender);
    result.grade = grade_for(body, school_id.as_deref(), season);
    let meet = result
        .meet_id
        .as_deref()
        .and_then(|id| body.get("meets")?.get(id));
    result.meet_name = meet
        .and_then(|m| v::text(m, "MeetName"))
        .or_else(|| v::text(row, "meetName"));
    if result.date.is_none() {
        result.date = meet_date(body, row);
    }
    result.course = if xc { v::text(row, "Course") } else { None };
    Some(result)
}

fn event_label(body: &Value, key: &str, row: &Value) -> Option<String> {
    if key == "resultsXC" {
        let distance = row.get("Distance")?.as_i64().filter(|n| *n > 0)?;
        return Some(format!("Cross Country {distance}m"));
    }
    let event_id = v::id(row, "EventID")?;
    let definition = body
        .get("eventsTF")?
        .as_array()?
        .iter()
        .find(|e| v::id(e, "IDEvent").as_deref() == Some(&event_id))?;
    v::text(definition, "Event")
}

fn unassigned(body: &Value, key: &str, row: &Value, requested: Sport) -> UnassignedResult {
    let sport = if key == "resultsXC" {
        Sport::CrossCountry
    } else {
        row_sport(body, row).map_or(requested, core::convert::identity)
    };
    let label = event_label(body, key, row).map_or(String::new(), core::convert::identity);
    let relay = is_relay(body, row, &label);
    let mut result = v::unassigned(row, label, sport, relay);
    if result.date.is_none() {
        result.date = meet_date(body, row);
    }
    result
}

fn retain_members(body: &Value, row: &Value, result: &mut UnassignedResult) {
    let team = row.get("AthleteID");
    let legs = body
        .get("relayTeamMembers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|leg| team.is_some() && leg.get("TeamID") == team);
    let mut raw = Vec::new();
    legs.for_each(|leg| {
        let member = v::id(leg, "AthleteID")
            .map(|id| format!("athleticnet:{id}"))
            .or_else(|| v::text(leg, "Name").filter(|s| !s.contains(['*', '<', '•'])));
        if let Some(member) = member {
            result.members.push(member);
        }
        raw.push(leg);
    });
    result.raw = serde_json::json!({"row":row,"relayLegs":raw});
}

fn meet_date(body: &Value, row: &Value) -> Option<String> {
    let id = v::id(row, "MeetID")?;
    body.get("meets")?
        .get(id)?
        .get("EndDate")?
        .as_str()
        .and_then(v::date)
}

fn row_sport(body: &Value, row: &Value) -> Option<Sport> {
    body.get("allSeasons")?
        .as_array()?
        .iter()
        .find(|s| {
            v::id(s, "SchoolID") == v::id(row, "SchoolID")
                && season_number(s, "IDSeason") == season_number(row, "SeasonID")
        })
        .and_then(season_sport)
}

fn grade_for(body: &Value, school: Option<&str>, season: Option<i32>) -> Option<String> {
    let key = format!("{}_{}", school?, season?);
    v::grade(body.get("grades")?, &key)
}

fn is_relay(body: &Value, row: &Value, label: &str) -> bool {
    let event = v::id(row, "EventID");
    label.to_ascii_lowercase().contains("relay")
        || body
            .get("eventsTF")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|e| {
                v::id(e, "IDEvent") == event
                    && e.get("PersonalEvent").and_then(Value::as_bool) == Some(false)
            })
}
