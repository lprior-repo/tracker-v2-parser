use std::collections::BTreeSet;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::model::{Athlete, PageContext, PageKind, ParsedPage, School, Sport, Target};

use super::{API, api_target, athlete_target, meet_target, values as v};

pub(super) fn target(
    id: &str,
    gender: &str,
    event: &str,
    page: u64,
    context: PageContext,
) -> Target {
    let mut target = api_target(
        PageKind::Directory,
        format!("{API}/tfRankings/GetRankings"),
        context,
    );
    target.method = Some("POST".to_owned());
    target.body = Some(json!({
        "reportType":"div", "mode":"list", "divListId":id.parse::<u64>().ok(),
        "indoor":null, "eventShort":event, "gender":gender,
        "qParams":{"page":page}, "qualifyingListKey":"", "version":2, "debug":""
    }));
    target
}

pub(super) fn parse(request: &Target, body: &Value) -> Result<ParsedPage> {
    let groups = v::array(body, "groupedRankings")?;
    let rows = groups
        .iter()
        .map(|group| {
            group
                .as_array()
                .context("Athletic.net rankings group is not an array")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut page = ParsedPage::default();
    let mut athletes = BTreeSet::new();
    let mut meets = BTreeSet::new();
    let masked = rows
        .iter()
        .flat_map(|group| group.iter())
        .filter(|row| v::flag(row.get("blurred")))
        .count();
    rows.iter()
        .flat_map(|group| group.iter())
        .filter(|row| !v::flag(row.get("blurred")))
        .try_for_each(|row| {
            retain_row(request, body, row, &mut athletes, &mut meets, &mut page)?;
            Ok::<(), anyhow::Error>(())
        })?;
    if masked > 0 {
        page.notes.push(format!("{masked} ranking rows are explicitly blurred/private; not an empty population and no identity was inferred. Pagination stopped at the public boundary."));
        return Ok(page);
    }
    let id = body
        .get("division")
        .and_then(|d| v::id(d, "ID"))
        .context("rankings division.ID missing")?;
    let gender = body
        .get("gender")
        .and_then(Value::as_str)
        .context("rankings gender missing")?;
    let event = body
        .get("eventShort")
        .and_then(Value::as_str)
        .context("rankings eventShort missing")?;
    if event.is_empty() {
        rows.iter()
            .flat_map(|group| group.iter())
            .filter_map(|row| v::text(row, "EventShort"))
            .collect::<BTreeSet<_>>()
            .iter()
            .for_each(|event| {
                page.next
                    .push(target(&id, gender, event, 1, request.context.clone()));
            });
    } else if let Some(number) = next_page(body, &rows) {
        page.next
            .push(target(&id, gender, event, number, request.context.clone()));
    }
    Ok(page)
}

fn retain_row(
    request: &Target,
    body: &Value,
    row: &Value,
    athletes: &mut BTreeSet<String>,
    meets: &mut BTreeSet<String>,
    page: &mut ParsedPage,
) -> Result<()> {
    let sport = request
        .context
        .sport
        .context("rankings target has no sport")?;
    let state =
        v::text(row, "State").and_then(|s| super::directory::state_code(&s).map(str::to_owned));
    let school = v::id(row, "TeamID");
    if let (Some(id), Some(name)) = (school.as_ref(), v::text(row, "TeamName")) {
        page.schools.push(School {
            id: id.clone(),
            name,
            state: state.clone(),
            ..School::default()
        });
    }
    let relay = row.get("PersonalEvent").and_then(Value::as_bool) == Some(false)
        || v::id(row, "IDResult").is_some_and(|id| {
            body.get("relayTeams")
                .and_then(|teams| teams.get(id))
                .is_some()
        });
    if !relay {
        retain_athlete(row, school, state, sport, athletes, page)?;
    }
    if let Some(id) = v::id(row, "MeetID").filter(|id| meets.insert(id.clone())) {
        let mut meet = meet_target(&id, sport)?;
        meet.context.state.clone_from(&request.context.state);
        page.next.push(meet);
    }
    Ok(())
}

fn retain_athlete(
    row: &Value,
    school: Option<String>,
    state: Option<String>,
    sport: Sport,
    seen: &mut BTreeSet<String>,
    page: &mut ParsedPage,
) -> Result<()> {
    let (Some(id), Some(name)) = (v::id(row, "AthleteID"), v::text(row, "AthleteName")) else {
        page.notes
            .push("Public ranking row has incomplete athlete identity".to_owned());
        return Ok(());
    };
    if !seen.insert(id.clone()) {
        return Ok(());
    }
    let next = athlete_target(&id, state.as_deref(), sport)?;
    page.athletes.push(Athlete {
        profile_url: Some(format!(
            "https://www.athletic.net/athlete/{id}/track-and-field"
        )),
        id,
        name,
        school_id: school,
        state,
        grade: v::grade(row, "GradeID"),
        ..Athlete::default()
    });
    page.next.push(next);
    Ok(())
}

fn next_page(body: &Value, rows: &[&Vec<Value>]) -> Option<u64> {
    let settings = body.get("settings")?;
    let current = settings.get("page")?.as_u64()?;
    let depth = settings.get("depth")?.as_u64()?.max(1);
    let count = body.get("minCount")?.as_u64()?;
    let row_count = u64::try_from(rows.iter().map(|group| group.len()).sum::<usize>()).ok()?;
    let last = rows
        .iter()
        .flat_map(|group| group.iter())
        .filter_map(|row| row.get("rowNum")?.as_u64())
        .max()?;
    if row_count < depth || last >= count {
        None
    } else {
        current.checked_add(1)
    }
}
