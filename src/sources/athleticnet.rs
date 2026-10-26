mod bio;
mod directory;
mod meet;
mod rankings;
mod team;
mod values;

#[cfg(test)]
mod tests;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

use crate::model::{PageContext, PageKind, ParsedPage, Source, Sport, Target};

const ORIGIN: &str = "https://www.athletic.net";
const API: &str = "https://www.athletic.net/api/v1";

pub fn parse(target: &Target, body: &str) -> Result<ParsedPage> {
    ensure!(
        target.source == Source::AthleticNet,
        "not an Athletic.net target"
    );
    let value: Value = serde_json::from_str(body).context("Athletic.net response is not JSON")?;
    match target.kind {
        PageKind::Athlete => bio::parse(target, &value),
        PageKind::Meet => meet::metadata(target, &value),
        PageKind::Results => meet::results(target, &value),
        PageKind::Directory => directory::parse(target, &value),
        PageKind::Team | PageKind::Roster => team::parse(target, &value),
        PageKind::Contacts => bail!("Athletic.net has no captured public contacts schema"),
    }
}

pub fn seeds(states: &[String], season: i32) -> Result<Vec<Target>> {
    ensure!((1900..=9999).contains(&season), "invalid season year");
    let filter = states
        .iter()
        .map(|state| {
            directory::state_code(state).with_context(|| format!("unknown state: {state}"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
    let context = PageContext {
        season: Some(season),
        subject_id: (!filter.is_empty()).then_some(filter),
        ..PageContext::default()
    };
    Ok(std::iter::once(api_target(
        PageKind::Directory,
        format!("{API}/public/GetStatesCountries2"),
        context.clone(),
    ))
    .chain([Sport::Outdoor, Sport::Indoor].into_iter().map(|sport| {
        let indoor = sport == Sport::Indoor;
        api_target(
            PageKind::Directory,
            format!("{API}/tfRankings/GetNavInfo?seasonId={season}&level=4&gender=m&recordSetId=0&locationId=0&teamId=0&indoor={indoor}"),
            PageContext { sport: Some(sport), ..context.clone() },
        )
    }))
    .collect())
}

pub fn athlete_target(id: &str, state: Option<&str>, sport: Sport) -> Result<Target> {
    validate_id(id)?;
    Ok(api_target(
        PageKind::Athlete,
        format!(
            "{API}/AthleteBio/GetAthleteBioData?athleteId={id}&sport={}&level=0",
            scope(sport)
        ),
        PageContext {
            athlete_id: Some(id.to_owned()),
            state: hint(state)?,
            sport: Some(sport),
            ..PageContext::default()
        },
    ))
}

pub fn team_target(id: &str, state: Option<&str>, sport: Sport, season: i32) -> Result<Target> {
    validate_id(id)?;
    ensure!((1900..=9999).contains(&season), "invalid season year");
    let (wire_sport, page_sport) = match sport {
        Sport::Outdoor => ("tfo", "track-and-field-outdoor"),
        Sport::Indoor => ("tfi", "track-and-field-indoor"),
        Sport::CrossCountry => ("xc", "cross-country"),
    };
    let mut target = api_target(
        PageKind::Team,
        format!("{API}/TeamHome/GetTeamCore?teamId={id}&sport={wire_sport}&year={season}"),
        PageContext {
            school_id: Some(id.to_owned()),
            state: hint(state)?,
            sport: Some(sport),
            season: Some(season),
            ..PageContext::default()
        },
    );
    target.browser_origin = Some(format!("{ORIGIN}/team/{id}/{page_sport}/{season}"));
    Ok(target)
}

pub fn meet_target(id: &str, sport: Sport) -> Result<Target> {
    validate_id(id)?;
    Ok(api_target(
        PageKind::Meet,
        format!("{API}/Meet/GetMeetData?meetId={id}&sport={}", scope(sport)),
        PageContext {
            subject_id: Some(id.to_owned()),
            sport: Some(sport),
            ..PageContext::default()
        },
    ))
}

fn api_target(kind: PageKind, url: String, context: PageContext) -> Target {
    Target {
        browser_origin: Some(ORIGIN.to_owned()),
        ..Target::get(Source::AthleticNet, kind, url, context)
    }
}

const fn scope(sport: Sport) -> &'static str {
    match sport {
        Sport::CrossCountry => "xc",
        Sport::Outdoor | Sport::Indoor => "tf",
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
        "invalid Athletic.net identity: {id}"
    );
    ensure!(
        id.parse::<u64>().is_ok_and(|n| n > 0),
        "invalid Athletic.net identity: {id}"
    );
    Ok(())
}

fn hint(state: Option<&str>) -> Result<Option<String>> {
    state
        .map(|raw| {
            directory::state_code(raw)
                .map(str::to_owned)
                .with_context(|| format!("unknown state: {raw}"))
        })
        .transpose()
}
