mod rows;

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::model::{Athlete, ParsedPage, School, Sport, Target};

use super::{athlete_target, meet_target, team_target, values as v};

pub(super) fn parse(target: &Target, body: &Value) -> Result<ParsedPage> {
    v::object(body, "athlete")?;
    let person = body.get("athlete").context("missing athlete")?;
    let identity =
        v::id(person, "IDAthlete").context("Athletic.net schema mismatch: athlete.IDAthlete")?;
    v::check_identity(target.context.athlete_id.as_deref(), &identity)?;
    let school_id = v::id(person, "SchoolID");
    let sport = target
        .context
        .sport
        .context("athlete target has no sport")?;
    let mut page = ParsedPage::default();
    let Some(mut athlete) = v::athlete(
        person,
        identity,
        school_id.clone(),
        target.context.state.clone(),
        sport,
    ) else {
        page.notes.push(
            "Athlete identity is private, masked, or incomplete; no person manufactured".to_owned(),
        );
        rows::retain_unidentified(body, sport, &mut page);
        return Ok(page);
    };
    retain_grade(body, &mut athlete);
    if athlete.gender.is_none() || athlete.grade.is_none() {
        page.notes.push("Athlete gender or current grade evidence is incomplete; missing facts were not inferred".to_owned());
    }
    schools(target, body, school_id.as_deref(), &mut page)?;
    rows::retain(body, "resultsTF", &athlete, sport, &mut page);
    rows::retain(body, "resultsXC", &athlete, sport, &mut page);
    let discovered = discover_meets(body, &page)?;
    page.next.extend(discovered);
    if sport != Sport::CrossCountry {
        page.next.push(athlete_target(
            &athlete.id,
            athlete.state.as_deref(),
            Sport::CrossCountry,
        )?);
    }
    if athlete.state.is_none() {
        page.notes.push(
            "Athlete state is not published in bio and no state hint was supplied".to_owned(),
        );
    }
    page.athletes.push(athlete);
    Ok(page)
}

fn retain_grade(body: &Value, athlete: &mut Athlete) {
    let latest = body
        .get("grades")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|map| map.iter())
        .filter_map(|(key, value)| {
            let (school, season) = key.rsplit_once('_')?;
            if athlete.school_id.as_deref() != Some(school) {
                return None;
            }
            let year = season.parse::<i32>().ok()?.rem_euclid(10_000);
            let grade = value.as_i64().filter(|n| (1..=12).contains(n))?;
            Some((year, grade))
        })
        .max_by_key(|(year, _)| *year);
    if let Some((_, grade)) = latest {
        athlete.grade = Some(grade.to_string());
    }
}

fn schools(
    target: &Target,
    body: &Value,
    current: Option<&str>,
    page: &mut ParsedPage,
) -> Result<()> {
    v::object(body, "allTeams")?
        .iter()
        .try_for_each(|(key, team)| {
            super::validate_id(key)?;
            if let Some(published) = v::id(team, "IDSchool") {
                v::check_identity(Some(key), &published)?;
            }
            let Some(name) = v::text(team, "SchoolName") else {
                page.notes
                    .push(format!("School {key} has no published name"));
                return Ok(());
            };
            let state = v::text(team, "State")
                .and_then(|s| super::directory::state_code(&s).map(str::to_owned))
                .or_else(|| {
                    (current == Some(key))
                        .then(|| target.context.state.clone())
                        .flatten()
                });
            page.schools.push(School {
                id: key.clone(),
                name,
                state: state.clone(),
                ..School::default()
            });
            let seasons = body
                .get("allSeasons")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|s| v::id(s, "SchoolID").as_deref() == Some(key))
                .filter_map(|s| {
                    Some((
                        season_sport(s)?,
                        season_number(s, "IDSeason")?.rem_euclid(10_000),
                    ))
                });
            seasons
                .map(|(sport, year)| team_target(key, state.as_deref(), sport, year))
                .try_for_each(|next| {
                    page.next.push(next?);
                    Ok::<(), anyhow::Error>(())
                })
        })
}

fn season_sport(season: &Value) -> Option<Sport> {
    let display = season.get("Display")?.as_str()?.to_ascii_lowercase();
    if display.contains("indoor") {
        Some(Sport::Indoor)
    } else if display.contains("outdoor") {
        Some(Sport::Outdoor)
    } else if display.contains("cross") {
        Some(Sport::CrossCountry)
    } else {
        None
    }
}

fn season_number(row: &Value, key: &str) -> Option<i32> {
    row.get(key)?.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn discover_meets(body: &Value, page: &ParsedPage) -> Result<Vec<Target>> {
    let rows = page
        .performances
        .iter()
        .filter_map(|row| Some((row.meet_id.as_deref()?, row.sport)))
        .chain(
            page.unassigned_results
                .iter()
                .filter_map(|row| Some((row.meet_id.as_deref()?, row.sport))),
        );
    let unique = rows.fold(std::collections::BTreeMap::new(), |mut map, (id, sport)| {
        map.entry(id).or_insert(sport);
        map
    });
    unique
        .into_iter()
        .filter(|(id, _)| body.get("meets").and_then(|m| m.get(*id)).is_some())
        .map(|(id, sport)| meet_target(id, sport))
        .collect()
}
