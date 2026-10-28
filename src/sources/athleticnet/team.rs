use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::model::{Athlete, PageContext, PageKind, ParsedPage, School, Target};

use super::{API, api_target, athlete_target, team_target, values as v};

pub(super) fn parse(target: &Target, body: &Value) -> Result<ParsedPage> {
    if target.kind == PageKind::Roster {
        return roster(target, body);
    }
    v::object(body, "team")?;
    let row = body.get("team").context("missing team")?;
    let id = v::id(row, "IDTeam")
        .or_else(|| v::id(row, "ID"))
        .context("Athletic.net schema mismatch: team identity")?;
    v::check_identity(target.context.school_id.as_deref(), &id)?;
    let Some(name) = v::text(row, "Name") else {
        return Ok(ParsedPage {
            notes: vec![format!("School {id} has a masked/incomplete name")],
            ..ParsedPage::default()
        });
    };
    let state = v::text(row, "State")
        .and_then(|s| super::directory::state_code(&s).map(str::to_owned))
        .or_else(|| target.context.state.clone());
    let school = School {
        id,
        name,
        state,
        city: v::text(row, "City"),
        address: v::text(row, "Address"),
        postal_code: v::text(row, "ZipCode"),
        website: v::text(row, "WebsiteSport").or_else(|| v::text(row, "Website")),
        ..School::default()
    };
    let context = PageContext {
        school_id: Some(school.id.clone()),
        school_name: Some(school.name.clone()),
        state: school.state.clone(),
        ..target.context.clone()
    };
    let next = if row.get("IDTeam").is_some() {
        vec![roster_target(target, body, context)?]
    } else {
        vec![team_target(
            &school.id,
            school.state.as_deref(),
            context.sport.context("team target lacks sport")?,
            context.season.context("team target lacks season")?,
        )?]
    };
    let next = next
        .into_iter()
        .map(|mut next| {
            next.context
                .acquisition_state
                .clone_from(&target.context.acquisition_state);
            next
        })
        .collect();
    Ok(ParsedPage {
        schools: vec![school],
        next,
        notes: vec![
            "Public Athletic.net team/coaches captures do not publish coach email addresses"
                .to_owned(),
        ],
        ..ParsedPage::default()
    })
}

fn roster_target(core: &Target, body: &Value, context: PageContext) -> Result<Target> {
    let season = body
        .get("seasonInfo")
        .and_then(|s| v::id(s, "seasonId"))
        .context("Athletic.net schema mismatch: seasonInfo.seasonId")?;
    let mut target = api_target(
        PageKind::Roster,
        format!("{API}/TeamHome/GetAthletes?seasonId={season}"),
        context,
    );
    target.browser_origin.clone_from(&core.browser_origin);
    target.token_url = Some(core.url.clone());
    Ok(target)
}

fn roster(target: &Target, body: &Value) -> Result<ParsedPage> {
    let rows = body
        .as_array()
        .context("Athletic.net roster schema mismatch: expected public athlete array")?;
    let sport = target.context.sport.context("roster target lacks sport")?;
    let mut page = ParsedPage::default();
    rows.iter().try_for_each(|row| {
        let (Some(id), Some(name)) = (
            v::id(row, "ID"),
            v::text(row, "Name").filter(|s| !s.contains(['*', '<', '•'])),
        ) else {
            page.notes.push(
                "Roster athlete is masked or incomplete; identity was not manufactured".to_owned(),
            );
            return Ok(());
        };
        let mut next = athlete_target(&id, target.context.state.as_deref(), sport)?;
        next.context
            .acquisition_state
            .clone_from(&target.context.acquisition_state);
        let profile = if sport == crate::model::Sport::CrossCountry {
            "cross-country"
        } else {
            "track-and-field"
        };
        let athlete = Athlete {
            profile_url: Some(format!("https://www.athletic.net/athlete/{id}/{profile}")),
            id,
            name,
            school_id: target.context.school_id.clone(),
            state: target.context.state.clone(),
            gender: v::gender(row.get("Gender").and_then(Value::as_str)),
            ..Athlete::default()
        };
        page.athletes.push(athlete);
        page.next.push(next);
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(page)
}
