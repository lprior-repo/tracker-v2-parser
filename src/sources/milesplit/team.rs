use anyhow::{Result, ensure};
use scraper::{ElementRef, Html};

use crate::model::{Athlete, PageContext, PageKind, ParsedPage, School, Source, Sport, Target};

use super::{html, targets};

pub(super) fn parse(target: &Target, document: &Html) -> Result<ParsedPage> {
    let root = document.root_element();
    let id = targets::path_id(&target.url, "teams")?;
    let canonical = html::attribute(root, "link[rel='canonical']", "href")?
        .ok_or_else(|| anyhow::anyhow!("school page missing canonical identity"))?;
    ensure!(
        targets::path_id(&canonical, "teams")? == id,
        "MileSplit school canonical identity mismatch"
    );
    if let Some(expected) = &target.context.school_id {
        ensure!(*expected == id, "MileSplit school target identity mismatch");
    }
    let school = school(target, root, id)?;
    let context = PageContext {
        state: school.state.clone(),
        school_id: Some(school.id.clone()),
        school_name: Some(school.name.clone()),
        ..target.context.clone()
    };
    let page = ParsedPage {
        schools: vec![school],
        ..ParsedPage::default()
    };
    let roster = root
        .select(&html::selector("#rosterDataset, .athlete-row")?)
        .next()
        .is_some();
    if roster
        || target.kind == PageKind::Roster
        || target.url.trim_end_matches('/').ends_with("/roster")
    {
        ensure!(roster, "unknown MileSplit roster template");
        return roster_rows(target, root, &context, page);
    }
    let href = html::attribute(
        root,
        ".navigation-profile a[href$='/roster'], a.navigation-link[href$='/roster']",
        "href",
    )?
    .ok_or_else(|| anyhow::anyhow!("school profile has no published roster link"))?;
    let url = targets::linked(&target.url, &href)?;
    ensure!(
        targets::path_id(&url, "teams")? == context.school_id.as_deref().map_or("", |value| value),
        "roster link belongs to another school"
    );
    Ok(ParsedPage {
        next: vec![Target::get(
            Source::MileSplit,
            PageKind::Roster,
            url,
            context,
        )],
        ..page
    })
}

fn school(target: &Target, root: ElementRef<'_>, id: String) -> Result<School> {
    let name = html::required(root, ".jumbotron-content h1")?;
    let spans = root
        .select(&html::selector(
            ".jumbotron-content .profile .info li span",
        )?)
        .map(html::text)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let mut locations = spans
        .iter()
        .filter(|value| html::location(value).1.is_some());
    let location = locations.next().filter(|_| locations.next().is_none());
    let (city, state) = location.map_or((None, None), |value| html::location(value));
    let postal_code = location.and_then(|value| html::postal(value));
    let mut streets = spans.iter().filter(|value| html::street(value));
    let address = streets.next().filter(|_| streets.next().is_none()).cloned();
    let website = root
        .select(&html::selector(
            ".jumbotron-content .profile .info a[href]",
        )?)
        .find(|link| html::text(*link).eq_ignore_ascii_case("website"))
        .and_then(|link| link.value().attr("href"))
        .map(str::to_owned);
    let public_email = html::attribute(
        root,
        ".jumbotron-content .profile .info a[href^='mailto:']",
        "href",
    )?
    .and_then(|href| {
        href.strip_prefix("mailto:").map(|email| {
            email
                .split('?')
                .next()
                .map_or("", core::convert::identity)
                .to_owned()
        })
    });
    Ok(School {
        id,
        name,
        state: state.or_else(|| target.context.state.clone()),
        city,
        address,
        website,
        postal_code,
        public_email,
        uuid: None,
    })
}

fn roster_rows(
    target: &Target,
    root: ElementRef<'_>,
    context: &PageContext,
    page: ParsedPage,
) -> Result<ParsedPage> {
    let rows = html::selector("li.athlete-row")?;
    let empty = root.select(&rows).next().is_none();
    let mut page = root
        .select(&rows)
        .try_fold(page, |page, row| roster_row(target, context, row, page))?;
    if empty {
        page.notes.push(
            "Published school roster is empty; no athlete absence inferred beyond this capture"
                .to_owned(),
        );
    }
    Ok(page)
}

fn roster_row(
    target: &Target,
    context: &PageContext,
    row: ElementRef<'_>,
    mut page: ParsedPage,
) -> Result<ParsedPage> {
    let (id, url, published) = roster_identity(target, row)?;
    let claim = html::value(row, ".column-grad-year")?;
    let grad = html::graduation_year(claim.as_deref())?;
    let gender = html::value(row, ".column-gender")?
        .as_deref()
        .and_then(html::gender);
    let sports = active_sports(row)?;
    let sport = sports
        .first()
        .copied()
        .or(context.sport)
        .map_or(Sport::Outdoor, |value| value);
    let next_context = PageContext {
        athlete_id: Some(id.clone()),
        sport: Some(sport),
        ..context.clone()
    };
    page.next.push(Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        url.clone(),
        next_context,
    ));
    if published.is_empty() {
        page.notes.push(format!(
            "Incomplete MileSplit roster observation: published athlete name missing; {}",
            serde_json::json!({
                "athlete_id": id,
                "profile_url": url,
                "school_id": context.school_id,
                "graduation_claim": claim,
                "graduation_year": grad,
                "gender": gender,
                "sports": sports,
                "row": row.html(),
            })
        ));
        return Ok(page);
    }
    let name = published.split_once(',').map_or_else(
        || published.clone(),
        |(last, first)| format!("{} {}", first.trim(), last.trim()),
    );
    page.athletes.push(Athlete {
        id,
        name,
        school_id: context.school_id.clone(),
        state: context.state.clone(),
        gender,
        graduation_year: grad,
        profile_url: Some(url),
        ..Athlete::default()
    });
    Ok(page)
}

fn roster_identity(target: &Target, row: ElementRef<'_>) -> Result<(String, String, String)> {
    row.select(&html::selector("a[href*='/athletes/']")?)
        .try_fold(None::<(String, String, String)>, |identity, link| {
            let href = link
                .value()
                .attr("href")
                .ok_or_else(|| anyhow::anyhow!("athlete URL missing"))?;
            let url = targets::linked(&target.url, href)?;
            let id = targets::path_id(&url, "athletes")?;
            let published = html::text(link);
            match identity {
                None => Ok(Some((id, url, published))),
                Some((previous, previous_url, name)) => {
                    ensure!(
                        previous == id,
                        "MileSplit roster row has conflicting athlete identities"
                    );
                    if name.is_empty() && !published.is_empty() {
                        Ok(Some((id, url, published)))
                    } else {
                        Ok(Some((previous, previous_url, name)))
                    }
                }
            }
        })?
        .ok_or_else(|| anyhow::anyhow!("MileSplit roster row missing athlete profile identity"))
}

fn active_sports(row: ElementRef<'_>) -> Result<Vec<Sport>> {
    row.select(&html::selector("[data-season-id]")?)
        .try_fold(Vec::new(), |mut sports, cell| {
            if cell
                .select(&html::selector("svg.icon-yes")?)
                .next()
                .is_some()
            {
                match cell.value().attr("data-season-id") {
                    Some("1") => sports.push(Sport::Indoor),
                    Some("2") => sports.push(Sport::Outdoor),
                    Some("3") => sports.push(Sport::CrossCountry),
                    _ => {}
                }
            }
            Ok(sports)
        })
}
