mod email;
mod links;
mod office;
mod postal;
mod staff;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::model::{PageContext, PageKind, ParsedPage, School, Source, Target};

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

pub fn targets(source: Source, school: &School, season: i32) -> Result<Vec<Target>> {
    let Some(website) = school
        .website
        .as_deref()
        .filter(|site| !site.trim().is_empty())
    else {
        return Ok(Vec::new());
    };
    ensure!(
        !school.id.trim().is_empty() && !school.name.trim().is_empty(),
        "contacts require school ID and name"
    );
    let url = public_url(website)?;
    Ok(vec![Target::get(
        source,
        PageKind::Contacts,
        url.into(),
        PageContext {
            school_id: Some(school.id.clone()),
            school_name: Some(school.name.clone()),
            state: school.state.clone(),
            season: Some(season),
            subject_id: Some("contacts-depth:0".to_owned()),
            ..PageContext::default()
        },
    )])
}

pub fn parse(target: &Target, body: &str) -> Result<ParsedPage> {
    ensure!(
        target.kind == PageKind::Contacts,
        "contacts parser requires contacts page kind"
    );
    ensure!(body.len() <= MAX_PAGE_BYTES, "contacts page exceeds 8 MiB");
    let school_id = target
        .context
        .school_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .context("contacts require school ID")?;
    let school_name = target
        .context
        .school_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .context("contacts require school name")?;
    public_url(&target.url)?;
    ensure!(
        body.contains('<') && body.contains('>'),
        "contacts response is not HTML"
    );
    let document = Html::parse_document(body);
    ensure!(
        document
            .select(&selector(
                "title, main, h1, a, address, script[type='application/ld+json']"
            )?)
            .next()
            .is_some(),
        "contacts response has no recognizable page content"
    );
    let coaches = staff::parse(&document, school_id)?;
    let schools = postal::parse(&document, target, school_name)?;
    let schools = office::enrich(&document, target, school_name, schools)?;
    let next = links::parse(&document, target)?;
    let mut notes = Vec::new();
    if coaches.is_empty() {
        notes.push("No unambiguous public track/XC coaching mailbox extracted; contact coverage remains unknown or incomplete.".to_owned());
    }
    if !schools.iter().any(|school| school.address.is_some()) {
        notes.push("No school-matched postal address extracted; address coverage remains unknown or incomplete.".to_owned());
    }
    if !schools.iter().any(|school| school.public_email.is_some()) {
        notes.push("No unambiguous public school/athletics office mailbox extracted; office contact coverage remains unknown or incomplete.".to_owned());
    }
    Ok(ParsedPage {
        schools,
        coaches,
        next,
        notes,
        ..ParsedPage::default()
    })
}

fn selector(css: &str) -> Result<Selector> {
    Selector::parse(css)
        .map_err(|error| anyhow::anyhow!("invalid contacts selector {css}: {error}"))
}

fn public_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid official school URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "official school URL must be HTTP(S) without credentials"
    );
    Ok(url)
}

fn text(element: ElementRef<'_>) -> String {
    element
        .descendants()
        .filter_map(|node| {
            let value = node.value().as_text()?;
            let hidden = node.ancestors().filter_map(ElementRef::wrap).any(|parent| {
                matches!(parent.value().name(), "script" | "style" | "noscript")
                    || parent.value().attr("hidden").is_some()
                    || parent.value().attr("aria-hidden") == Some("true")
            });
            (!hidden).then_some(value.text.as_ref())
        })
        .flat_map(str::split_whitespace)
        .fold(String::new(), |mut collected, word| {
            if !collected.is_empty() {
                collected.push(' ');
            }
            collected.push_str(word);
            collected
        })
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}
