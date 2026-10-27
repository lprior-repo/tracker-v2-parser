use anyhow::{Result, ensure};
use scraper::{ElementRef, Html};
use serde_json::Value;

use crate::model::{
    Athlete, PageContext, PageKind, ParsedPage, Performance, School, Source, Target,
};

use super::{html, targets};

pub(super) fn parse(target: &Target, document: &Html) -> Result<ParsedPage> {
    let root = document.root_element();
    let id = targets::path_id(&target.url, "athletes")?;
    let canonical = html::attribute(root, "link[rel='canonical']", "href")?
        .ok_or_else(|| anyhow::anyhow!("MileSplit athlete missing canonical identity"))?;
    ensure!(
        targets::path_id(&canonical, "athletes")? == id,
        "MileSplit profile canonical identity mismatch"
    );
    let person = html::schema(document, "Person")?;
    let name = html::required(root, "#athleteName")?;
    let location = html::value(root, ".athlete-info .city-state")?;
    let (city, state) = location.as_deref().map_or((None, None), html::location);
    let school = profile_school(target, root, city, state.clone())?;
    let state = state.or_else(|| target.context.state.clone());
    let graduation_claim = html::value(root, ".athlete-info .grad-year")?;
    let graduation_year = html::graduation_year(
        graduation_claim
            .as_deref()
            .and_then(|value| value.strip_prefix("Class of ")),
    )?;
    let gender = progression_gender(root)?.or_else(|| {
        person
            .as_ref()
            .and_then(|person| html::field(person, "gender"))
            .as_deref()
            .and_then(html::gender)
    });
    let same_as = same_as(person.as_ref(), &id)?;
    let athlete = Athlete {
        id,
        name,
        school_id: school
            .as_ref()
            .map(|school| school.id.clone())
            .or_else(|| target.context.school_id.clone()),
        state,
        gender,
        graduation_year,
        profile_url: Some(canonical),
        same_as,
        grade: None,
        needs_review: false,
    };
    let mut page = ParsedPage {
        schools: school.into_iter().collect(),
        ..ParsedPage::default()
    };
    let table = root
        .select(&html::selector("#prTable")?)
        .next()
        .ok_or_else(|| anyhow::anyhow!("MileSplit profile has no recognized PR table"))?;
    page = table
        .select(&html::selector("tr[data-pr-mode]")?)
        .try_fold(page, |page, row| record(&athlete, row, page))?;
    page = table
        .select(&html::selector("tr[data-pr-mode='all']")?)
        .try_fold(page, headline)?;
    if html::masked(root)? {
        page.notes.push("Incomplete public access: masked athlete historical results retained in capture; public PRs only, no zero marks or absence inferred".to_owned());
    }
    if page.performances.is_empty() && page.notes.is_empty() {
        page.notes
            .push("Published athlete PR table contains no personal records".to_owned());
    }
    enqueue_school(target, root, &athlete, &mut page)?;
    page.athletes.push(athlete);
    Ok(page)
}

fn progression_gender(root: ElementRef<'_>) -> Result<Option<String>> {
    Ok(root.select(&html::selector("script")?).find_map(|script| {
        let body = script.inner_html();
        if !body.contains("progressionTrackerData") {
            return None;
        }
        body.lines().find_map(|line| {
            let assignment = line.trim().strip_prefix("var gender")?;
            let value = assignment.trim_start().strip_prefix('=')?.trim();
            let value = value.trim_end_matches(';').trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .or_else(|| {
                    value
                        .strip_prefix('\'')
                        .and_then(|value| value.strip_suffix('\''))
                })?;
            html::gender(value)
        })
    }))
}

fn profile_school(
    target: &Target,
    root: ElementRef<'_>,
    city: Option<String>,
    state: Option<String>,
) -> Result<Option<School>> {
    let Some(link) = root
        .select(&html::selector(".athlete-info .current-school a[href]")?)
        .next()
    else {
        return Ok(None);
    };
    let url = targets::linked(
        &target.url,
        link.value()
            .attr("href")
            .ok_or_else(|| anyhow::anyhow!("profile school missing link"))?,
    )?;
    let id = targets::path_id(&url, "teams")?;
    Ok(Some(School {
        id,
        name: html::text(link),
        city,
        state: state.or_else(|| target.context.state.clone()),
        ..School::default()
    }))
}

fn same_as(person: Option<&Value>, athlete_id: &str) -> Result<Vec<String>> {
    let Some(person) = person else {
        return Ok(Vec::new());
    };
    let url = person
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Person schema missing athlete URL"))?;
    ensure!(
        targets::path_id(url, "athletes")? == athlete_id,
        "Person schema belongs to another athlete"
    );
    Ok(match person.get("sameAs") {
        Some(Value::String(url)) => vec![url.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    })
}

fn record(athlete: &Athlete, row: ElementRef<'_>, mut page: ParsedPage) -> Result<ParsedPage> {
    let mode = row
        .value()
        .attr("data-pr-mode")
        .ok_or_else(|| anyhow::anyhow!("PR mode missing"))?;
    if mode == "all" {
        return Ok(page);
    }
    let level = row
        .value()
        .attr("data-pr-level")
        .map_or("all", |value| value);
    if level != "all" {
        return Ok(page);
    }
    let sport = targets::sport(mode)?;
    let cells = row.select(&html::selector("td")?).collect::<Vec<_>>();
    ensure!(
        cells.len() == 5,
        "unexpected MileSplit personal record column count"
    );
    let event = cells
        .first()
        .copied()
        .map(html::text)
        .ok_or_else(|| anyhow::anyhow!("PR event missing"))?;
    let mark_cell = cells
        .get(1)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("PR mark missing"))?;
    let mark = html::text(mark_cell);
    ensure!(
        !event.is_empty(),
        "MileSplit personal record event is blank"
    );
    if html::masked(mark_cell)?
        || mark.is_empty()
        || mark.chars().all(|ch| matches!(ch, '-' | '—' | '*'))
    {
        page.notes.push(format!(
            "Incomplete public access: masked PR {mode} {event}; source mark not available"
        ));
        return Ok(page);
    }
    let date = cells
        .get(4)
        .copied()
        .map(html::text)
        .filter(|date| !date.is_empty() && date != "-")
        .map(|date| html::date(&date));
    let meet_url = html::attribute(row, "a[href*='/meets/']", "href")?;
    let meet_id = meet_url
        .as_deref()
        .map(|url| targets::path_id(url, "meets"))
        .transpose()?;
    let meet_name = html::value(row, "a[href*='/meets/']")?;
    let result_id = Some(format!("pr:{}:{mode}:{level}:{event}", athlete.id));
    let is_relay = html::relay(&event);
    let performance = Performance {
        athlete_id: athlete.id.clone(),
        event,
        mark,
        reported_status: None,
        source_values: None,
        sport,
        result_id,
        source_row: None,
        school_id: athlete.school_id.clone(),
        meet_id,
        meet_name,
        date,
        wind: None,
        timing: None,
        course: None,
        is_personal_record: true,
        is_relay,
        season: None,
        grade: None,
        gender: athlete.gender.clone(),
    };
    page.performances.push(performance);
    Ok(page)
}

fn headline(mut page: ParsedPage, row: ElementRef<'_>) -> Result<ParsedPage> {
    let event = html::required(row, ".pr-td-event")?;
    let mark = html::value(row, ".pr-td-mark")?.map_or(String::new(), core::convert::identity);
    if !page
        .performances
        .iter()
        .any(|performance| performance.event == event && performance.mark == mark)
    {
        page.notes.push(format!("Incomplete sport evidence: all-season headline PR event={event:?} mark={mark:?}; source publishes no corresponding sport-specific PR, retained without guessing sport"));
    }
    Ok(page)
}

fn enqueue_school(
    target: &Target,
    root: ElementRef<'_>,
    athlete: &Athlete,
    page: &mut ParsedPage,
) -> Result<()> {
    let Some(school) = page.schools.first() else {
        return Ok(());
    };
    let href = html::attribute(root, ".athlete-info .current-school a[href]", "href")?
        .ok_or_else(|| anyhow::anyhow!("profile school link missing"))?;
    let url = targets::linked(&target.url, &href)?;
    let context = PageContext {
        school_id: Some(school.id.clone()),
        school_name: Some(school.name.clone()),
        state: athlete.state.clone(),
        athlete_id: None,
        ..target.context.clone()
    };
    page.next
        .push(Target::get(Source::MileSplit, PageKind::Team, url, context));
    Ok(())
}
