use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::model::{PageContext, PageKind, ParsedPage, School, Sport, Target};

use super::{API, api_target, team_target, values as v};

pub(super) fn parse(target: &Target, body: &Value) -> Result<ParsedPage> {
    if body.get("groupedRankings").is_some() {
        return super::rankings::parse(target, body);
    }
    if body.get("regions").is_some() {
        return navigation(target, body);
    }
    if body.get("states").is_some() {
        v::array(body, "states")?;
        return Ok(ParsedPage { notes: vec!["State/country vocabulary captured; school discovery uses the published navigation division IDs".to_owned()], ..ParsedPage::default() });
    }
    if body.get("divInfo").is_some() {
        return divisions(target, body);
    }
    bail!(
        "Athletic.net directory schema mismatch: expected states, regions, divInfo, or groupedRankings"
    )
}

fn navigation(target: &Target, body: &Value) -> Result<ParsedPage> {
    let mut page = ParsedPage::default();
    v::array(body, "regions")?
        .iter()
        .filter(|r| r.get("divType").and_then(Value::as_str) == Some("State"))
        .try_for_each(|region| {
            let Some(state) =
                v::text(region, "state").and_then(|s| state_code(&s).map(str::to_owned))
            else {
                page.notes.push(format!(
                    "Out-of-scope region {:?} has no US state code",
                    v::text(region, "name")
                ));
                return Ok(());
            };
            if !selected(target, &state) {
                return Ok(());
            }
            let id = v::id(region, "id").context("navigation state has no id")?;
            let context = PageContext {
                state: Some(state),
                subject_id: Some(id.clone()),
                ..target.context.clone()
            };
            page.next.push(division_target(&id, context.clone()));
            for gender in ["m", "f"] {
                page.next
                    .push(super::rankings::target(&id, gender, "", 1, context.clone()));
            }
            Ok::<(), anyhow::Error>(())
        })?;
    page.notes.push("Directory and public rankings are discovery surfaces, not a guarantee of complete athlete enumeration; masked rankings remain unavailable".to_owned());
    Ok(page)
}

fn selected(target: &Target, state: &str) -> bool {
    match target.context.subject_id.as_deref() {
        Some(filter) if filter.split(',').any(|code| state_code(code).is_some()) => {
            filter.split(',').any(|code| code == state)
        }
        _ => !matches!(state, "AK" | "HI"),
    }
}

fn divisions(target: &Target, body: &Value) -> Result<ParsedPage> {
    let rows = v::array(body, "divInfo")?;
    let mut page = ParsedPage::default();
    let state = target.context.state.clone().or_else(|| {
        rows.iter().find_map(|row| {
            v::text(row, "StateName").and_then(|s| state_code(&s).map(str::to_owned))
        })
    });
    rows.iter().try_for_each(|row| {
        let Some(id) = v::id(row, "DivIDDivision") else {
            return Ok(());
        };
        let child_state = state
            .clone()
            .or_else(|| v::text(row, "DivDivName").and_then(|s| state_code(&s).map(str::to_owned)));
        if state.is_none() && child_state.as_deref().is_none_or(|s| !selected(target, s)) {
            return Ok(());
        }
        page.next.push(division_target(
            &id,
            PageContext {
                state: child_state,
                subject_id: Some(id.clone()),
                ..target.context.clone()
            },
        ));
        Ok::<(), anyhow::Error>(())
    })?;
    v::array(body, "teams")?.iter().try_for_each(|row| {
        let (Some(id), Some(name)) = (v::id(row, "IDSchool"), v::text(row, "SchoolName")) else {
            page.notes
                .push("Directory school has incomplete identity/name".to_owned());
            return Ok(());
        };
        let Some(state) = state.as_deref() else {
            page.notes.push(format!(
                "School {id} has no published state association at this tree node"
            ));
            return Ok(());
        };
        page.schools.push(School {
            id: id.clone(),
            name,
            state: Some(state.to_owned()),
            ..School::default()
        });
        let year = target
            .context
            .season
            .context("directory target lacks season")?;
        [Sport::Outdoor, Sport::Indoor, Sport::CrossCountry]
            .into_iter()
            .map(|sport| team_target(&id, Some(state), sport, year))
            .try_for_each(|next| {
                page.next.push(next?);
                Ok::<(), anyhow::Error>(())
            })
    })?;
    Ok(page)
}

fn division_target(id: &str, context: PageContext) -> Target {
    api_target(
        PageKind::Directory,
        format!("{API}/SiteHeader/GetDivChildren?sport=tf&divId={id}"),
        context,
    )
}

pub(super) fn state_code(raw: &str) -> Option<&'static str> {
    STATES
        .iter()
        .find(|(code, name)| raw.eq_ignore_ascii_case(code) || raw.eq_ignore_ascii_case(name))
        .map(|(code, _)| *code)
}

const STATES: [(&str, &str); 51] = [
    ("AL", "Alabama"),
    ("AK", "Alaska"),
    ("AZ", "Arizona"),
    ("AR", "Arkansas"),
    ("CA", "California"),
    ("CO", "Colorado"),
    ("CT", "Connecticut"),
    ("DE", "Delaware"),
    ("DC", "District of Columbia"),
    ("FL", "Florida"),
    ("GA", "Georgia"),
    ("HI", "Hawaii"),
    ("ID", "Idaho"),
    ("IL", "Illinois"),
    ("IN", "Indiana"),
    ("IA", "Iowa"),
    ("KS", "Kansas"),
    ("KY", "Kentucky"),
    ("LA", "Louisiana"),
    ("ME", "Maine"),
    ("MD", "Maryland"),
    ("MA", "Massachusetts"),
    ("MI", "Michigan"),
    ("MN", "Minnesota"),
    ("MS", "Mississippi"),
    ("MO", "Missouri"),
    ("MT", "Montana"),
    ("NE", "Nebraska"),
    ("NV", "Nevada"),
    ("NH", "New Hampshire"),
    ("NJ", "New Jersey"),
    ("NM", "New Mexico"),
    ("NY", "New York"),
    ("NC", "North Carolina"),
    ("ND", "North Dakota"),
    ("OH", "Ohio"),
    ("OK", "Oklahoma"),
    ("OR", "Oregon"),
    ("PA", "Pennsylvania"),
    ("RI", "Rhode Island"),
    ("SC", "South Carolina"),
    ("SD", "South Dakota"),
    ("TN", "Tennessee"),
    ("TX", "Texas"),
    ("UT", "Utah"),
    ("VT", "Vermont"),
    ("VA", "Virginia"),
    ("WA", "Washington"),
    ("WV", "West Virginia"),
    ("WI", "Wisconsin"),
    ("WY", "Wyoming"),
];
