mod detail;
mod directory;

use anyhow::{Context, Result, ensure};
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::model::{PageContext, PageKind, ParsedPage, School, Source, Target};

pub use directory::{
    DirectoryMatch, DirectorySchool, listing_requests, match_school, parse_listing,
};

const ORIGIN: &str = "https://schools.wiaawi.org";
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[must_use]
pub fn is_school_url(value: &str) -> bool {
    school_url(value).is_ok()
}

#[must_use]
pub fn is_public_directory_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if url.as_str() != value
        || value.contains('%')
        || url.scheme() != "https"
        || url.host_str() != Some("schools.wiaawi.org")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    match url.path() {
        "/Directory/School/List" => url.query().is_none(),
        "/Directory/School/DirectoryLetter" => {
            let pairs = url.query_pairs().collect::<Vec<_>>();
            matches!(pairs.as_slice(), [(key, letter)] if key == "LetterBtn" && letter.len() == 1 && letter.bytes().all(|byte| byte.is_ascii_uppercase()))
        }
        "/Directory/School/GetDirectorySchool" => is_school_url(value),
        _ => false,
    }
}

pub fn contact_target(
    source: Source,
    school: &School,
    directory: &DirectorySchool,
    season: i32,
) -> Result<Target> {
    ensure!(
        matches!(
            match_school(school, std::slice::from_ref(directory)),
            DirectoryMatch::Matched(_)
        ),
        "WIAA contact target requires exact Wisconsin school name and city corroboration"
    );
    ensure!(!school.id.trim().is_empty(), "school source ID is required");
    let url = school_url(&directory.url)?;
    ensure!(
        url.query_pairs()
            .next()
            .is_some_and(|(_, id)| id == directory.organization_id),
        "WIAA directory orgID conflicts with its published URL"
    );
    Ok(Target::get(
        source,
        PageKind::Contacts,
        directory.url.clone(),
        PageContext {
            school_id: Some(school.id.clone()),
            school_name: Some(school.name.clone()),
            state: school.state.clone(),
            season: Some(season),
            subject_id: Some("contacts-depth:2".to_owned()),
            ..PageContext::default()
        },
    ))
}

pub fn parse(target: &Target, body: &str) -> Result<ParsedPage> {
    school_url(&target.url)?;
    ensure!(body.len() <= MAX_PAGE_BYTES, "WIAA page exceeds 8 MiB");
    super::contacts::check_public_access(body, &target.url)?;
    let school = detail::parse(target, body)?;
    let document = Html::parse_document(body);
    let tables = selector("table#tblCoachList")?;
    let tables = document.select(&tables).collect::<Vec<_>>();
    let markup = match tables.as_slice() {
        [] => "<main></main>".to_owned(),
        [table] => format!("<main>{}</main>", table.html()),
        _ => anyhow::bail!("WIAA coach table is ambiguous"),
    };
    let mut page = super::contacts::parse(target, &markup)?;
    page.next.clear();
    page.schools = vec![school];
    page.coaches.iter_mut().for_each(|coach| {
        coach
            .review_notes
            .retain(|note| !note.starts_with("School affiliation unconfirmed:"));
    });
    page.notes
        .retain(|note| !note.starts_with("No school-matched postal address extracted;"));
    Ok(page)
}

fn school_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid WIAA school URL")?;
    let pairs = url.query_pairs().collect::<Vec<_>>();
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("schools.wiaawi.org")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path() == "/Directory/School/GetDirectorySchool"
            && matches!(pairs.as_slice(), [(key, id)] if key.eq_ignore_ascii_case("orgID") && valid_id(id)),
        "WIAA school URL must contain exactly one published positive orgID"
    );
    Ok(url)
}

fn valid_id(value: &str) -> bool {
    !value.starts_with('0')
        && value.len() <= 10
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && !value.is_empty()
}

fn selector(value: &str) -> Result<Selector> {
    Selector::parse(value).map_err(|error| anyhow::anyhow!("WIAA selector {value}: {error}"))
}

fn text(element: ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
