use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::model::{
    Athlete, PageContext, PageKind, ParsedPage, Performance, School, Source, Target,
    UnassignedResult,
};

use super::{html, targets};

const FIELDS: &str = "id,meetId,teamId,teamName,athleteId,firstName,lastName,gender,divisionId,divisionName,meetResultsId,meetResultsDivisionId,resultsDivisionId,gradYear,eventName,eventCode,eventDistance,round,roundName,heat,units,mark,place,windReading,profileUrl,statusCode";

pub(super) fn target(
    parent: &Target,
    context: &PageContext,
    result_sets: Vec<String>,
) -> Result<Target> {
    ensure!(
        !result_sets.is_empty(),
        "structured result file IDs missing"
    );
    result_sets
        .iter()
        .try_for_each(|id| targets::id(id).map(|_| ()))?;
    let meet = targets::id(context.subject_id.as_deref().context("meet ID missing")?)?;
    let mut url = targets::public_url(&parent.url)?;
    ensure!(
        url.scheme() == "https",
        "structured result acquisition requires HTTPS"
    );
    url.set_path(&format!("/api/v1/meets/{meet}/performances"));
    url.set_query(Some(&format!("isMeetPro=0&fields={FIELDS}")));
    url.set_fragment(None);
    Ok(Target::get(
        Source::MileSplit,
        PageKind::Results,
        url.to_string(),
        PageContext {
            result_set_ids: result_sets,
            ..context.clone()
        },
    ))
}

pub(super) fn parse(target: &Target, body: &str) -> Result<ParsedPage> {
    let meet = targets::id(
        target
            .context
            .subject_id
            .as_deref()
            .context("structured result meet ID missing")?,
    )?;
    ensure!(
        !target.context.result_set_ids.is_empty(),
        "structured result file IDs missing"
    );
    target
        .context
        .result_set_ids
        .iter()
        .try_for_each(|id| targets::id(id).map(|_| ()))?;
    let url = Url::parse(&target.url)?;
    let host = url.host_str().context("structured result host missing")?;
    let site = host
        .strip_suffix(".milesplit.com")
        .context("invalid structured result host")?;
    ensure!(
        (site == "www" || targets::state(site).is_ok())
            && url.scheme() == "https"
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "invalid structured result URL"
    );
    ensure!(
        url.path() == format!("/api/v1/meets/{meet}/performances"),
        "structured result URL meet mismatch"
    );
    let document: Value =
        serde_json::from_str(body).context("MileSplit structured results are not JSON")?;
    let status = document
        .pointer("/_meta/status_code")
        .and_then(Value::as_u64);
    if matches!(status, Some(401 | 402 | 403 | 429 | 451)) {
        return Err(crate::sources::AccessDenied.into());
    }
    ensure!(
        status == Some(200),
        "MileSplit structured results provider status is not 200"
    );
    if let Some(embedded) = document.pointer("/_embedded/meet/id") {
        ensure!(
            scalar_id(Some(embedded)).as_deref() == Some(meet),
            "structured results embedded meet mismatch"
        );
    }
    let rows = document
        .get("data")
        .and_then(Value::as_array)
        .context("structured results missing data array")?;
    let sport = target
        .context
        .sport
        .context("structured results missing sport")?;
    let mut page = ParsedPage::default();
    let mut seen = BTreeSet::new();
    rows.iter().enumerate().try_for_each(|(index, row)| {
        let row_meet = scalar_id(row.get("meetId"));
        let row_set = scalar_id(row.get("meetResultsId"));
        let bound = row_meet.as_deref() == Some(meet)
            && row_set
                .as_ref()
                .is_some_and(|id| target.context.result_set_ids.contains(id));
        retain(target, row, index, sport, bound, &mut seen, &mut page)
    })?;
    page.notes.push(
        "Structured meet response has no verified published total; completeness is unknown"
            .to_owned(),
    );
    Ok(page)
}

fn scalar_id(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    targets::id(&text).ok().map(str::to_owned)
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
}

fn retain(
    target: &Target,
    row: &Value,
    index: usize,
    sport: crate::model::Sport,
    bound: bool,
    seen: &mut BTreeSet<String>,
    page: &mut ParsedPage,
) -> Result<()> {
    let event = text(row, "eventName");
    let mark = text(row, "mark");
    let result_id = scalar_id(row.get("id"));
    let athlete_id = scalar_id(row.get("athleteId"));
    let (graduation_year, needs_review) =
        review_graduation(row, athlete_id.as_deref(), &mut page.notes);
    let profile = text(row, "profileUrl");
    let name = person_name(row);
    let relay = event.is_some_and(html::relay);
    let valid_profile = athlete_id
        .as_deref()
        .zip(profile)
        .is_some_and(|(id, profile)| {
            targets::public_url(profile).ok().is_some_and(|url| {
                url.scheme() == "https"
                    && url.path().starts_with("/athletes/")
                    && targets::path_id(profile, "athletes").ok().as_deref() == Some(id)
            })
        });
    let unique = result_id.as_ref().is_some_and(|id| seen.insert(id.clone()));
    if !bound
        || relay
        || !valid_profile
        || !unique
        || name.is_none()
        || event.is_none()
        || mark.is_none()
    {
        retain_unassigned(target, row, index, sport, result_id, relay, page)?;
        return Ok(());
    }
    let (Some(id), Some(profile), Some(name), Some(event), Some(mark)) =
        (athlete_id, profile, name, event, mark)
    else {
        return Ok(());
    };
    let school = scalar_id(row.get("teamId")).zip(text(row, "teamName"));
    let school_id = school.as_ref().map(|(id, _)| id.clone());
    if let Some((id, name)) = school {
        page.schools.push(School {
            id,
            name: name.to_owned(),
            ..School::default()
        });
    }
    let gender = text(row, "gender").and_then(html::gender);
    page.athletes.push(Athlete {
        id: id.clone(),
        name,
        school_id: school_id.clone(),
        gender: gender.clone(),
        graduation_year,
        needs_review,
        profile_url: Some(profile.to_owned()),
        ..Athlete::default()
    });
    page.next.push(Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        profile.to_owned(),
        PageContext {
            athlete_id: Some(id.clone()),
            school_id: school_id.clone(),
            state: None,
            school_name: None,
            result_set_ids: Vec::new(),
            ..target.context.clone()
        },
    ));
    let status = text(row, "statusCode").and_then(crate::marks::failure_status);
    page.performances.push(Performance {
        athlete_id: id,
        event: event.to_owned(),
        mark: mark.to_owned(),
        reported_status: status.map(str::to_owned),
        source_values: Some(row.clone()),
        sport,
        result_id,
        source_row: Some(format!("data[{index}]")),
        school_id,
        meet_id: target.context.subject_id.clone(),
        meet_name: target.context.meet_name.clone(),
        date: target.context.meet_date.clone(),
        wind: text(row, "windReading")
            .and_then(|wind| wind.parse::<f64>().ok())
            .filter(|wind| wind.is_finite()),
        timing: None,
        course: None,
        is_personal_record: false,
        is_relay: false,
        season: target.context.season,
        grade: None,
        gender,
    });
    Ok(())
}

fn person_name(row: &Value) -> Option<String> {
    let first = text(row, "firstName").map_or("", |value| value);
    let last = text(row, "lastName").map_or("", |value| value);
    let name = format!("{first} {last}").trim().to_owned();
    (!name.is_empty() && !name.contains(['*', '•', '<', '>'])).then_some(name)
}

fn graduation_year(row: &Value) -> Result<Option<i32>> {
    match row.get("gradYear") {
        None | Some(Value::Null) => html::graduation_year(None),
        Some(Value::String(value)) => html::graduation_year(Some(value)),
        Some(Value::Number(value)) => html::graduation_year(Some(&value.to_string())),
        Some(_) => anyhow::bail!("MileSplit graduation year is not a string or number"),
    }
}

fn review_graduation(
    row: &Value,
    athlete_id: Option<&str>,
    notes: &mut Vec<String>,
) -> (Option<i32>, bool) {
    match graduation_year(row) {
        Ok(year) => (year, false),
        Err(error) => {
            let claim = row
                .get("gradYear")
                .map_or_else(|| "null".to_owned(), Value::to_string);
            notes.push(format!(
                "MileSplit athlete {} has unsupported graduation claim {claim}: {error}",
                athlete_id.map_or("unknown", |id| id)
            ));
            (None, true)
        }
    }
}

fn retain_unassigned(
    target: &Target,
    row: &Value,
    index: usize,
    sport: crate::model::Sport,
    result_id: Option<String>,
    relay: bool,
    page: &mut ParsedPage,
) -> Result<()> {
    page.unassigned_results.push(UnassignedResult {
        result_id,
        source_row: Some(format!(
            "data[{index}]:{:x}",
            Sha256::digest(serde_json::to_vec(row)?)
        )),
        event: text(row, "eventName").map_or_else(String::new, str::to_owned),
        mark: text(row, "mark").map_or_else(String::new, str::to_owned),
        sport,
        school_id: None,
        meet_id: scalar_id(row.get("meetId")),
        date: (scalar_id(row.get("meetId")).as_deref() == target.context.subject_id.as_deref())
            .then(|| target.context.meet_date.clone())
            .flatten(),
        is_relay: relay,
        members: Vec::new(),
        raw: row.clone(),
    });
    page.notes.push(format!("Structured result data[{index}] rejected: ownership, result-set binding, or row evidence incomplete"));
    Ok(())
}
