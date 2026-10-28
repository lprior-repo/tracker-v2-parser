use std::collections::BTreeMap;

use anyhow::Result;
use scraper::{ElementRef, Html};
use sha2::{Digest, Sha256};

use super::{html, targets};
use crate::model::{PageKind, Source, Target};

pub(super) struct Identity {
    pub(super) id: String,
    pub(super) basis: &'static str,
}

pub(super) fn identities(
    target: &Target,
    athlete_id: &str,
    rows: &[ElementRef<'_>],
) -> Result<Vec<Identity>> {
    let candidates = rows
        .iter()
        .map(|row| candidate(target, athlete_id, *row))
        .collect::<Result<Vec<_>>>()?;
    let counts =
        candidates
            .iter()
            .flatten()
            .fold(BTreeMap::<&str, usize>::new(), |mut counts, (id, _)| {
                let count = counts.entry(id.as_str()).or_default();
                *count = count.saturating_add(1);
                counts
            });
    Ok(rows
        .iter()
        .zip(&candidates)
        .enumerate()
        .map(|(index, (row, candidate))| {
            match candidate
                .as_ref()
                .filter(|(id, _)| counts.get(id.as_str()) == Some(&1))
            {
                Some((id, basis)) => Identity {
                    id: id.clone(),
                    basis,
                },
                None => Identity {
                    id: format!(
                        "profile-history:{athlete_id}:{:x}:ambiguous:{index}",
                        Sha256::digest(row.html().as_bytes())
                    ),
                    basis: "ambiguous",
                },
            }
        })
        .collect())
}

pub fn retained_identity(
    target: &Target,
    athlete_id: &str,
    body: &str,
) -> Result<Option<(String, &'static str)>> {
    if target.source != Source::MileSplit
        || target.kind != PageKind::Athlete
        || targets::path_id(&target.url, "athletes")? != athlete_id
    {
        return Ok(None);
    }
    let document = Html::parse_fragment(body);
    let root = document.root_element();
    let mut elements = root.children().filter_map(ElementRef::wrap);
    let Some(row) = elements.next() else {
        return Ok(None);
    };
    if elements.next().is_some()
        || row.value().name() != "div"
        || !row
            .value()
            .attr("class")
            .is_some_and(|classes| classes.split_whitespace().any(|class| class == "record"))
        || row.value().attr("data-season").is_none()
        || row
            .value()
            .attr("data-event")
            .is_none_or(|event| event.trim().is_empty())
    {
        return Ok(None);
    }
    let Some(mark) = html::value(row, ".seed")? else {
        return Ok(None);
    };
    let Some(mode) = row.value().attr("data-season") else {
        return Ok(None);
    };
    if mark.is_empty() || html::masked(row)? || targets::sport(mode).is_err() {
        return Ok(None);
    }
    candidate(target, athlete_id, row)
}

fn candidate(
    target: &Target,
    athlete_id: &str,
    row: ElementRef<'_>,
) -> Result<Option<(String, &'static str)>> {
    if let Some(id) = row.value().attr("data-result-id") {
        return Ok(Some((
            format!("profile-result:{}", targets::id(id)?),
            "published_result",
        )));
    }
    let Some(href) = html::attribute(row, ".location a[href*='/meets/']", "href")? else {
        return Ok(None);
    };
    let meet = targets::path_id(&targets::linked(&target.url, &href)?, "meets")?;
    let Some(date) = html::value(row, ".date")?.filter(|date| !date.trim().is_empty()) else {
        return Ok(None);
    };
    let date = html::date(&date);
    if chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_err() {
        return Ok(None);
    }
    let Some(event) = row
        .value()
        .attr("data-event")
        .filter(|event| !event.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(mode) = row.value().attr("data-season") else {
        return Ok(None);
    };
    let Ok(sport) = targets::sport(mode) else {
        return Ok(None);
    };
    let round = html::value(row, ".round")?;
    let context = serde_json::to_vec(&(athlete_id, sport.as_str(), event, meet, date, round))?;
    Ok(Some((
        format!(
            "profile-history:{athlete_id}:context:{:x}",
            Sha256::digest(context)
        ),
        "unique_context",
    )))
}
