use anyhow::{Result, ensure};
use scraper::{ElementRef, Html};

use crate::model::{PageKind, ParsedPage, School, Source, Target};

use super::{html, targets};

pub(super) fn schools(target: &Target, document: &Html) -> Result<ParsedPage> {
    let root = document.root_element();
    ensure!(
        root.select(&html::selector("table.teams")?)
            .next()
            .is_some(),
        "unknown MileSplit school directory template"
    );
    let selected = html::attribute(root, "#teamType option[selected]", "value")?;
    ensure!(
        selected.as_deref().is_none_or(|value| value == "1"),
        "MileSplit directory is not high school"
    );
    let page = root
        .select(&html::selector("table.teams tr")?)
        .try_fold(ParsedPage::default(), |page, row| {
            school_row(target, row, page)
        })?;
    ensure!(
        !page.schools.is_empty() || !page.notes.is_empty(),
        "MileSplit school directory has no readable rows"
    );
    Ok(page)
}

fn school_row(target: &Target, row: ElementRef<'_>, mut page: ParsedPage) -> Result<ParsedPage> {
    let Some(link) = row.select(&html::selector("td a[href*='/teams/']")?).next() else {
        return Ok(page);
    };
    let href = link
        .value()
        .attr("href")
        .ok_or_else(|| anyhow::anyhow!("school link missing URL"))?;
    let url = targets::linked(&target.url, href)?;
    let id = targets::path_id(&url, "teams")?;
    let location = row.select(&html::selector("td")?).nth(1).map(html::text);
    let (city, published_state) = location.as_deref().map_or((None, None), html::location);
    if location
        .as_deref()
        .is_some_and(|value| value.contains(',') && published_state.is_none())
    {
        page.notes.push(format!("Foreign or unrecognized school location retained only in capture: team {id}, {location:?}"));
        return Ok(page);
    }
    let school = School {
        id: id.clone(),
        name: html::text(link),
        state: published_state.or_else(|| target.context.state.clone()),
        city,
        postal_code: location.as_deref().and_then(html::postal),
        ..School::default()
    };
    ensure!(!school.name.is_empty(), "MileSplit school {id} has no name");
    let context = crate::model::PageContext {
        school_id: Some(id),
        school_name: Some(school.name.clone()),
        state: school.state.clone(),
        ..target.context.clone()
    };
    page.next
        .push(Target::get(Source::MileSplit, PageKind::Team, url, context));
    page.schools.push(school);
    Ok(page)
}

pub(super) fn meets(target: &Target, document: &Html) -> Result<ParsedPage> {
    let root = document.root_element();
    ensure!(
        root.select(&html::selector(
            "#frmFilter[action='/results'], .meet-month, .meet-row"
        )?)
        .next()
        .is_some(),
        "unknown MileSplit meet index template"
    );
    let mut page = root
        .select(&html::selector("section.meet-month")?)
        .try_fold(ParsedPage::default(), |page, month| {
            meet_month(target, month, page)
        })?;
    let published_rows = root.select(&html::selector("li.meet-row")?).count();
    ensure!(
        page.next.len() == published_rows,
        "MileSplit meet rows are outside recognized month sections"
    );
    let next = root
        .select(&html::selector("a[rel~='next'], link[rel~='next']")?)
        .next()
        .and_then(|link| link.value().attr("href"));
    if let Some(href) = next {
        let url = targets::linked(&target.url, href)?;
        let mut parsed = targets::public_url(&url)?;
        preserve_query(target, &mut parsed)?;
        ensure!(
            parsed.as_str() != target.url,
            "MileSplit pagination points to current page"
        );
        page.next.push(Target::get(
            Source::MileSplit,
            PageKind::Directory,
            parsed.to_string(),
            target.context.clone(),
        ));
    }
    if page.next.is_empty() {
        page.notes
            .push("Public meet index contains no published meets for this filter".to_owned());
    }
    Ok(page)
}

fn preserve_query(target: &Target, url: &mut url::Url) -> Result<()> {
    let original = targets::public_url(&target.url)?;
    for wanted in ["season", "level", "year"] {
        let value = original
            .query_pairs()
            .find(|(key, _)| key == wanted)
            .map(|(_, value)| value);
        if let Some(value) = value.filter(|_| !url.query_pairs().any(|(key, _)| key == wanted)) {
            url.query_pairs_mut().append_pair(wanted, &value);
        }
    }
    Ok(())
}

fn meet_month(target: &Target, month: ElementRef<'_>, page: ParsedPage) -> Result<ParsedPage> {
    month
        .select(&html::selector("li.meet-row")?)
        .try_fold(page, |mut page, row| {
            let id = row
                .value()
                .attr("data-meet-id")
                .ok_or_else(|| anyhow::anyhow!("meet row missing ID"))?;
            targets::id(id)?;
            let name = html::required(row, "a.meet-row__name")?;
            let href = html::attribute(row, "a.meet-row__name", "href")?
                .ok_or_else(|| anyhow::anyhow!("meet row missing link"))?;
            let mut url = targets::public_url(&targets::linked(&target.url, &href)?)?;
            let path = url.path().trim_end_matches('/');
            if !path.ends_with("/results") {
                url.set_path(&format!("{path}/results"));
            }
            ensure!(
                targets::path_id(url.as_str(), "meets")? == id,
                "MileSplit meet identity mismatch"
            );
            let date = month
                .value()
                .attr("data-month")
                .zip(html::value(row, ".meet-row__day")?)
                .and_then(|(bucket, day)| meet_date(bucket, &day));
            let context = crate::model::PageContext {
                subject_id: Some(id.to_owned()),
                meet_name: Some(name),
                meet_date: date,
                school_id: None,
                school_name: None,
                athlete_id: None,
                ..target.context.clone()
            };
            page.next.push(Target::get(
                Source::MileSplit,
                PageKind::Meet,
                url.to_string(),
                context,
            ));
            Ok(page)
        })
}

fn meet_date(month: &str, day: &str) -> Option<String> {
    let day = day
        .split_whitespace()
        .next_back()?
        .trim_end_matches(|ch: char| !ch.is_ascii_digit())
        .parse::<u32>()
        .ok()?;
    let (year, month) = month.split_once('-')?;
    chrono::NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, day)
        .map(|date| date.format("%Y-%m-%d").to_string())
}
