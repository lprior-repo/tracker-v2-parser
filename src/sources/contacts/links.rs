use std::collections::BTreeSet;

use anyhow::{Context, Result};
use regex::Regex;
use scraper::Html;
use url::Url;

use crate::model::{PageKind, Target};

use super::{public_url, selector, text};

pub(super) fn parse(document: &Html, target: &Target) -> Result<Vec<Target>> {
    let depth = depth(target)?;
    if depth >= 2 {
        return Ok(Vec::new());
    }
    let mut document_url = public_url(&target.url)?;
    document_url.set_fragment(None);
    let bases = selector("base[href]")?;
    let declared_base = document
        .select(&bases)
        .next()
        .and_then(|base| base.value().attr("href"))
        .and_then(|href| document_url.join(href).ok());
    let base = declared_base
        .as_ref()
        .map_or(&document_url, core::convert::identity);
    let interesting =
        Regex::new(r"(?i)coach|athletic|staff|directory|contact|cross[- ]?country|\btrack\b")?;
    let anchors = selector("a[href]")?;
    Ok(document
        .select(&anchors)
        .filter_map(|anchor| {
            let href = anchor.value().attr("href")?;
            let label = text(anchor);
            let url = base.join(href).ok()?;
            (interesting.is_match(url.path()) || interesting.is_match(&label))
                .then_some((url, label))
        })
        .filter(|(url, label)| followable(&document_url, url, label, &interesting))
        .map(|(mut url, _)| {
            url.set_fragment(None);
            url.to_string()
        })
        .filter(|url| url != document_url.as_str())
        .scan(BTreeSet::new(), |seen, url| {
            Some(seen.insert(url.clone()).then_some(url))
        })
        .flatten()
        .take(20)
        .map(|url| {
            let mut context = target.context.clone();
            context.subject_id = Some(format!("contacts-depth:{}", depth.saturating_add(1)));
            Target::get(target.source, PageKind::Contacts, url, context)
        })
        .collect())
}

fn depth(target: &Target) -> Result<u8> {
    target
        .context
        .subject_id
        .as_deref()
        .map_or(Ok(0), |subject| {
            subject
                .strip_prefix("contacts-depth:")
                .context("invalid contacts crawl depth context")?
                .parse::<u8>()
                .context("invalid contacts crawl depth")
        })
}

fn followable(base: &Url, url: &Url, label: &str, interesting: &Regex) -> bool {
    if public_url(url.as_str()).is_err()
        || url.path().rsplit('.').next().is_some_and(|extension| {
            ["pdf", "jpg", "png", "zip", "ics", "xlsx", "docx", "mp4"]
                .iter()
                .any(|blocked| extension.eq_ignore_ascii_case(blocked))
        })
    {
        return false;
    }
    base.origin() == url.origin() || (staff_platform(url) && interesting.is_match(label))
}

fn staff_platform(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    [
        "bigteams.com",
        "arbiterwebsites.com",
        "arbitersports.com",
        "rschooltoday.com",
        "rschoolteams.com",
        "homecampus.com",
        "fpsports.org",
        "8to18.com",
        "sportngin.com",
        "sportsengine.com",
        "teampages.com",
        "finalsite.com",
        "schoolwires.net",
        "edlio.com",
        "apptegy.com",
        "thrillshare.com",
        "sites.google.com",
        "sidearmsports.com",
    ]
    .iter()
    .any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}
