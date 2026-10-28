mod access;
#[cfg(test)]
mod claim_tests;
mod email;
mod links;
mod office;
mod postal;
pub mod recovery;
mod staff;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::model::{PageContext, PageKind, ParsedPage, School, Source, Target};

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct AccessDenied;

impl std::fmt::Display for AccessDenied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("Official school website access challenge; no alternate transport attempted")
    }
}

impl std::error::Error for AccessDenied {}

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
    let url = official_url(website)?;
    if !website.contains("://") {
        tracing::warn!(
            source = source.as_str(),
            school_id = %school.id,
            published_website = website,
            request_url = %url,
            "Assuming HTTPS for published scheme-less school website; original school website retained"
        );
    }
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

pub(crate) fn check_public_access(body: &str, url: &str) -> Result<()> {
    ensure!(body.len() <= MAX_PAGE_BYTES, "contacts page exceeds 8 MiB");
    access::check(&Html::parse_document(body), url)
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
    access::check(&document, &target.url)?;
    ensure!(
        document
            .select(&selector(
                "title, main, h1, a, address, script[type='application/ld+json']"
            )?)
            .next()
            .is_some(),
        "contacts response has no recognizable page content"
    );
    let coaches = staff::parse(&document, school_id, school_name)?;
    let schools = postal::parse(&document, target, school_name)?;
    let schools = office::enrich(&document, target, school_name, schools)?;
    let next = links::parse(&document, target)?;
    let mut notes = Vec::new();
    if coaches.is_empty() {
        notes.push("No unambiguous public track/XC coaching mailbox extracted; contact coverage remains unknown or incomplete.".to_owned());
    }
    if coaches.iter().any(|coach| coach.email.is_none()) {
        notes.push("Named public track/XC coaching claim retained without an unambiguous public mailbox; email remains unpublished or ambiguous.".to_owned());
    }
    if coaches.iter().any(|coach| coach.name.is_none()) {
        notes.push("Public track/XC coaching mailbox retained without a published name; identity requires review.".to_owned());
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

fn official_url(value: &str) -> Result<Url> {
    ensure!(
        !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
            && !value.contains('\\'),
        "invalid official school URL: whitespace, control characters, or backslashes are ambiguous"
    );
    if let Some((scheme, authority)) = value.split_once("://") {
        ensure!(
            (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
                && !authority.starts_with('/'),
            "invalid official school URL: malformed or unsupported scheme"
        );
        return public_url(value);
    }
    ensure!(
        scheme_less_hostname(value),
        "invalid official school URL: scheme-less website must be an unambiguous hostname and optional path"
    );
    public_url(&format!("https://{value}"))
}

fn scheme_less_hostname(value: &str) -> bool {
    if value.contains([':', '@']) {
        return false;
    }
    let host = value
        .split(['/', '?', '#'])
        .next()
        .map_or("", std::convert::identity);
    let Some((_, suffix)) = host.rsplit_once('.') else {
        return false;
    };
    host.len() <= 253
        && suffix.len() >= 2
        && suffix.bytes().all(|byte| byte.is_ascii_alphabetic())
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
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
    crate::transport::policy::check_url_secrets(&url)?;
    Ok(url)
}

fn text(element: ElementRef<'_>) -> String {
    text_content(element, &[])
}

fn staff_text(element: ElementRef<'_>) -> String {
    text_content(element, &["footer", "header", "nav"])
}

fn text_content(element: ElementRef<'_>, excluded: &[&str]) -> String {
    element
        .descendants()
        .filter_map(|node| {
            let value = node.value().as_text()?;
            let parent = node.ancestors().find_map(ElementRef::wrap)?;
            let hidden = email::hidden(parent)
                || excluded.contains(&parent.value().name())
                || parent
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .any(|ancestor| excluded.contains(&ancestor.value().name()));
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
