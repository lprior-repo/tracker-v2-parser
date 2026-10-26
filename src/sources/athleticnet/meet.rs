use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::model::{
    Athlete, PageContext, PageKind, ParsedPage, Performance, School, Sport, Target,
    UnassignedResult,
};

use super::{API, api_target, athlete_target, scope, values as v};

pub(super) fn metadata(target: &Target, body: &Value) -> Result<ParsedPage> {
    v::object(body, "meet")?;
    let meet = body.get("meet").context("missing meet")?;
    let id = v::id(meet, "ID").context("Athletic.net schema mismatch: meet.ID")?;
    v::check_identity(target.context.subject_id.as_deref(), &id)?;
    let sport = match body.get("sport2").and_then(Value::as_str) {
        Some("tfi") => Sport::Indoor,
        Some("tfo") => Sport::Outdoor,
        Some("xc") => Sport::CrossCountry,
        _ => target.context.sport.context("meet target has no sport")?,
    };
    let context = PageContext {
        subject_id: Some(id.clone()),
        sport: Some(sport),
        meet_name: v::text(meet, "Name"),
        meet_date: meet
            .get("MeetDate")
            .and_then(Value::as_str)
            .and_then(v::date),
        season: meet
            .get("SeasonID")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok()),
        ..target.context.clone()
    };
    let mut results = api_target(
        PageKind::Results,
        format!(
            "{API}/Meet/GetAllResultsData?meetId={id}&sport={}&rawResults=false&showTips=false",
            scope(sport)
        ),
        context,
    );
    results.token_url = Some(target.url.clone());
    Ok(ParsedPage {
        next: vec![results],
        ..ParsedPage::default()
    })
}

pub(super) fn results(target: &Target, body: &Value) -> Result<ParsedPage> {
    let blocks = v::array(body, "flatEvents")?;
    let teams = v::array(body, "teams")?;
    let sport = target
        .context
        .sport
        .context("results target has no sport")?;
    let legs = relay_index(body);
    let mut page = ParsedPage::default();
    let mut schools = BTreeMap::new();
    for team in teams {
        retain_school(team, "IDSchool", &mut schools);
    }
    let mut athletes = BTreeMap::new();
    blocks.iter().enumerate().try_for_each(|(ordinal, block)| {
        retain_block(
            target,
            &legs,
            block,
            ordinal,
            &mut athletes,
            &mut schools,
            &mut page,
        )
    })?;
    page.schools = schools.into_values().collect();
    page.athletes = athletes.into_values().collect();
    page.athletes
        .iter()
        .map(|a| athlete_target(&a.id, a.state.as_deref(), sport))
        .try_for_each(|next| {
            page.next.push(next?);
            Ok::<(), anyhow::Error>(())
        })?;
    if target.context.meet_date.is_none() {
        page.notes
            .push("Meet date absent from results context; dates were not manufactured".to_owned());
    }
    Ok(page)
}

fn relay_index(body: &Value) -> BTreeMap<u64, Vec<&Value>> {
    body.get("relayLegs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|leg| Some((leg.get("ResultID")?.as_u64()?, leg)))
        .fold(BTreeMap::new(), |mut index, (id, leg)| {
            index.entry(id).or_default().push(leg);
            index
        })
}

fn retain_school(row: &Value, key: &str, schools: &mut BTreeMap<String, School>) {
    if let (Some(id), Some(name)) = (v::id(row, key), v::text(row, "SchoolName")) {
        schools.entry(id.clone()).or_insert_with(|| School {
            id,
            name,
            ..School::default()
        });
    }
}

fn retain_block(
    target: &Target,
    legs: &BTreeMap<u64, Vec<&Value>>,
    block: &Value,
    block_ordinal: usize,
    athletes: &mut BTreeMap<String, Athlete>,
    schools: &mut BTreeMap<String, School>,
    page: &mut ParsedPage,
) -> Result<()> {
    let label =
        v::text(block, "Event").context("Athletic.net schema mismatch: flatEvents.Event")?;
    v::array(block, "results")?
        .iter()
        .enumerate()
        .for_each(|(ordinal, row)| {
            let performances = page.performances.len();
            let unassigned = page.unassigned_results.len();
            retain_school(row, "TeamID", schools);
            let relay = label.to_ascii_lowercase().contains("relay")
                || block.get("isPersonal").and_then(Value::as_bool) == Some(false)
                || row.get("PersonalEvent").and_then(Value::as_bool) == Some(false);
            if relay {
                retain_relay(target, legs, block, row, &label, athletes, page);
            } else {
                retain_individual(target, block, row, &label, athletes, page);
            }
            let source_row =
                v::source_row(row, format!("flatEvents:{block_ordinal}:results:{ordinal}"));
            if page.performances.len() > performances {
                if let Some(result) = page.performances.last_mut() {
                    result.source_row = Some(source_row);
                }
            } else if page.unassigned_results.len() > unassigned
                && let Some(result) = page.unassigned_results.last_mut()
            {
                result.source_row = Some(source_row);
            }
        });
    Ok(())
}

fn retain_individual(
    target: &Target,
    block: &Value,
    row: &Value,
    label: &str,
    athletes: &mut BTreeMap<String, Athlete>,
    page: &mut ParsedPage,
) {
    let Some(sport) = target.context.sport else {
        return;
    };
    let athlete = v::id(row, "AthleteID")
        .and_then(|id| v::athlete(row, id, v::id(row, "TeamID"), None, sport));
    let Some(mut athlete) = athlete else {
        page.notes.push(format!(
            "Result {:?} has masked/incomplete athlete identity",
            v::id(row, "IDResult")
        ));
        page.unassigned_results
            .push(unassigned(target, row, label, sport, false));
        return;
    };
    if athlete.gender.is_none() {
        athlete.gender = v::gender(block.get("Gender").and_then(Value::as_str));
    }
    let performance = v::performance(row, athlete.id.clone(), label.to_owned(), sport)
        .map(|p| contextualize(target, &athlete, p));
    athletes.entry(athlete.id.clone()).or_insert(athlete);
    if let Some(result) = performance {
        page.performances.push(result);
    } else {
        page.notes.push(format!(
            "Result {:?} has masked/incomplete mark",
            v::id(row, "IDResult")
        ));
        page.unassigned_results
            .push(unassigned(target, row, label, sport, false));
    }
}

fn contextualize(target: &Target, athlete: &Athlete, mut result: Performance) -> Performance {
    result.meet_id.clone_from(&target.context.subject_id);
    result.meet_name.clone_from(&target.context.meet_name);
    result.date.clone_from(&target.context.meet_date);
    result.season = target.context.season;
    result.grade.clone_from(&athlete.grade);
    result.gender.clone_from(&athlete.gender);
    result
}

fn retain_relay(
    target: &Target,
    index: &BTreeMap<u64, Vec<&Value>>,
    block: &Value,
    row: &Value,
    label: &str,
    athletes: &mut BTreeMap<String, Athlete>,
    page: &mut ParsedPage,
) {
    let Some(sport) = target.context.sport else {
        return;
    };
    let legs = row
        .get("IDResult")
        .and_then(Value::as_u64)
        .and_then(|id| index.get(&id));
    let mut result = unassigned(target, row, label, sport, true);
    let mut retained = BTreeSet::new();
    legs.into_iter().flatten().copied().for_each(|leg| {
        let identity = v::id(leg, "AthleteID");
        let name = v::text(leg, "Name").filter(|s| !s.contains(['*', '<', '•']));
        if let Some(member) = identity
            .as_ref()
            .map(|id| format!("athleticnet:{id}"))
            .or_else(|| name.clone())
        {
            retained.insert(member);
        }
        let (Some(id), Some(name)) = (identity, name) else {
            return;
        };
        let athlete = Athlete {
            profile_url: Some(format!(
                "https://www.athletic.net/athlete/{id}/track-and-field"
            )),
            id: id.clone(),
            name,
            school_id: v::id(row, "TeamID"),
            gender: v::gender(block.get("Gender").and_then(Value::as_str)),
            grade: v::grade(leg, "ShortDesc"),
            ..Athlete::default()
        };
        athletes.entry(id).or_insert(athlete);
    });
    if retained.is_empty() {
        page.notes.push(format!("Relay result {:?} has no published athlete legs; relay-team identity was not made a person", result.result_id));
    }
    result.members = retained.into_iter().collect();
    result.raw = serde_json::json!({"row":row,"relayLegs":legs});
    page.unassigned_results.push(result);
}

fn unassigned(
    target: &Target,
    row: &Value,
    label: &str,
    sport: Sport,
    is_relay: bool,
) -> UnassignedResult {
    let mut result = v::unassigned(row, label.to_owned(), sport, is_relay);
    result.meet_id.clone_from(&target.context.subject_id);
    result.date.clone_from(&target.context.meet_date);
    result
}
