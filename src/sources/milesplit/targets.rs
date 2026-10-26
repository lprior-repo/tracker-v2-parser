use anyhow::{Result, bail, ensure};
use url::Url;

use crate::model::{PageContext, PageKind, Source, Sport, Target};

const STATES: &str = "AL AZ AR CA CO CT DE DC FL GA ID IL IN IA KS KY LA ME MD MA MI MN MS MO MT NE NV NH NJ NM NY NC ND OH OK OR PA RI SC SD TN TX UT VT VA WA WV WI WY";
const SPORTS: [Sport; 3] = [Sport::CrossCountry, Sport::Indoor, Sport::Outdoor];

pub(super) fn state(value: &str) -> Result<String> {
    let code = value.trim().to_ascii_uppercase();
    ensure!(
        STATES.split_whitespace().any(|candidate| candidate == code),
        "unsupported lower-48 state {value:?}"
    );
    Ok(code)
}

pub(super) fn id(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && !value.starts_with('0')
            && value.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid positive MileSplit ID {value:?}"
    );
    ensure!(value.parse::<u64>()? > 0, "invalid zero MileSplit ID");
    Ok(value)
}

fn host(state_code: Option<&str>) -> Result<String> {
    state_code.map_or_else(
        || Ok("www.milesplit.com".to_owned()),
        |code| {
            Ok(format!(
                "{}.milesplit.com",
                state(code)?.to_ascii_lowercase()
            ))
        },
    )
}

pub(super) const fn season_code(sport: Sport) -> &'static str {
    match sport {
        Sport::CrossCountry => "cc",
        Sport::Outdoor => "outdoor",
        Sport::Indoor => "indoor",
    }
}

pub(super) fn public_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    let hostname = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("MileSplit URL has no host"))?;
    let site = hostname
        .strip_suffix(".milesplit.com")
        .ok_or_else(|| anyhow::anyhow!("not a MileSplit host"))?;
    ensure!(
        site == "www" || state(site).is_ok(),
        "unsupported MileSplit site {site}"
    );
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "invalid public MileSplit URL"
    );
    ensure!(
        !url.path().starts_with("/api/")
            && !url.path().starts_with("/login")
            && !url.path().starts_with("/join"),
        "authenticated MileSplit path refused"
    );
    Ok(url)
}

pub(super) fn linked(base: &str, href: &str) -> Result<String> {
    let resolved = public_url(base)?.join(href)?;
    public_url(resolved.as_str())?;
    Ok(resolved.to_string())
}

pub(super) fn path_id(value: &str, label: &str) -> Result<String> {
    let url = public_url(value)?;
    let mut parts = url
        .path_segments()
        .ok_or_else(|| anyhow::anyhow!("URL has no path"))?;
    let raw = parts
        .find(|part| *part == label)
        .and_then(|_| parts.next())
        .ok_or_else(|| anyhow::anyhow!("URL has no {label} identity"))?;
    let digits = raw
        .split('-')
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing source ID"))?;
    Ok(id(digits)?.to_owned())
}

pub fn seeds(states: &[String], season: i32) -> Result<Vec<Target>> {
    ensure!((1900..=2200).contains(&season), "invalid season {season}");
    states.iter().try_fold(Vec::new(), |mut targets, value| {
        let state = state(value)?;
        let host = host(Some(&state))?;
        let context = PageContext {
            state: Some(state),
            season: Some(season),
            ..PageContext::default()
        };
        targets.push(Target::get(
            Source::MileSplit,
            PageKind::Directory,
            format!("https://{host}/teams"),
            context.clone(),
        ));
        targets.extend(SPORTS.map(|sport| {
            Target::get(
                Source::MileSplit,
                PageKind::Directory,
                format!(
                    "https://{host}/results?season={}&level=hs&year={season}&page=1",
                    season_code(sport)
                ),
                PageContext {
                    sport: Some(sport),
                    ..context.clone()
                },
            )
        }));
        Ok(targets)
    })
}

pub fn athlete_target(athlete_id: &str, state_code: Option<&str>, sport: Sport) -> Result<Target> {
    let id = id(athlete_id)?;
    Ok(Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://{}/athletes/{id}", host(state_code)?),
        PageContext {
            athlete_id: Some(id.to_owned()),
            state: state_code.map(state).transpose()?,
            sport: Some(sport),
            ..PageContext::default()
        },
    ))
}

pub fn team_target(
    team_id: &str,
    state_code: Option<&str>,
    sport: Sport,
    season: i32,
) -> Result<Target> {
    ensure!((1900..=2200).contains(&season), "invalid season {season}");
    let id = id(team_id)?;
    Ok(Target::get(
        Source::MileSplit,
        PageKind::Team,
        format!("https://{}/teams/{id}", host(state_code)?),
        PageContext {
            school_id: Some(id.to_owned()),
            state: state_code.map(state).transpose()?,
            sport: Some(sport),
            season: Some(season),
            ..PageContext::default()
        },
    ))
}

pub fn meet_target(meet_id: &str, sport: Sport) -> Result<Target> {
    let id = id(meet_id)?;
    Ok(Target::get(
        Source::MileSplit,
        PageKind::Meet,
        format!("https://www.milesplit.com/meets/{id}/results"),
        PageContext {
            subject_id: Some(id.to_owned()),
            sport: Some(sport),
            ..PageContext::default()
        },
    ))
}

pub(super) fn sport(value: &str) -> Result<Sport> {
    let value = value.to_ascii_lowercase();
    if matches!(value.as_str(), "cc" | "xc") || value.contains("cross") {
        return Ok(Sport::CrossCountry);
    }
    if value.contains("indoor") {
        return Ok(Sport::Indoor);
    }
    if value.contains("outdoor") || value.contains("track") {
        return Ok(Sport::Outdoor);
    }
    bail!("unrecognized MileSplit sport {value:?}")
}
