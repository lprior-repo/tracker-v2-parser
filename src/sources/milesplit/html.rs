use anyhow::Result;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

pub(super) fn selector(css: &str) -> Result<Selector> {
    Selector::parse(css)
        .map_err(|error| anyhow::anyhow!("invalid MileSplit selector {css:?}: {error}"))
}

pub(super) fn text(element: ElementRef<'_>) -> String {
    element
        .text()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn value(root: ElementRef<'_>, css: &str) -> Result<Option<String>> {
    Ok(root
        .select(&selector(css)?)
        .next()
        .map(text)
        .filter(|value| !value.is_empty()))
}

pub(super) fn attribute(root: ElementRef<'_>, css: &str, name: &str) -> Result<Option<String>> {
    Ok(root
        .select(&selector(css)?)
        .next()
        .and_then(|element| element.value().attr(name))
        .map(str::to_owned))
}

pub(super) fn required(root: ElementRef<'_>, css: &str) -> Result<String> {
    value(root, css)?.ok_or_else(|| anyhow::anyhow!("MileSplit page missing required {css}"))
}

pub(super) fn graduation_year(value: Option<&str>) -> Result<Option<i32>> {
    let year = value
        .map(str::trim)
        .filter(|value| !matches!(*value, "" | "-" | "--" | "0"))
        .map(str::parse::<i32>)
        .transpose()?;
    anyhow::ensure!(
        year.is_none_or(|year| (1900..=2200).contains(&year)),
        "MileSplit graduation year is outside the supported range"
    );
    Ok(year)
}

pub(super) fn schema(document: &Html, kind: &str) -> Result<Option<Value>> {
    document
        .select(&selector("script[type='application/ld+json']")?)
        .try_fold(None, |found, script| {
            let value: Value = serde_json::from_str(&script.inner_html())?;
            Ok(found.or_else(|| schema_node(&value, kind).cloned()))
        })
}

fn schema_node<'a>(value: &'a Value, kind: &str) -> Option<&'a Value> {
    if value.get("@type").and_then(Value::as_str) == Some(kind) {
        return Some(value);
    }
    value
        .as_array()
        .or_else(|| value.get("@graph").and_then(Value::as_array))?
        .iter()
        .find(|node| node.get("@type").and_then(Value::as_str) == Some(kind))
}

pub(super) fn field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned())
}

pub(super) fn location(value: &str) -> (Option<String>, Option<String>) {
    let value = domestic_location(value);
    let Some((city, rest)) = value.rsplit_once(',') else {
        return (None, super::targets::state(value.trim()).ok());
    };
    let code = rest
        .split_whitespace()
        .next()
        .and_then(|code| super::targets::state(code).ok());
    let foreign = rest
        .split_whitespace()
        .skip(1)
        .any(|token| token != "USA" && token != "US" && !postal_token(token));
    if foreign {
        return (Some(city.trim().to_owned()), None);
    }
    (Some(city.trim().to_owned()), code)
}

pub(super) fn street(value: &str) -> bool {
    if value.contains(',') {
        return false;
    }
    let mut parts = value.split_whitespace();
    let Some(number) = parts.next() else {
        return false;
    };
    if number.eq_ignore_ascii_case("po") || number.eq_ignore_ascii_case("p.o.") {
        return parts
            .next()
            .is_some_and(|part| part.eq_ignore_ascii_case("box"))
            && parts.next().is_some_and(|part| {
                part.chars().any(|character| character.is_ascii_digit())
                    && part
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
            })
            && parts.next().is_none();
    }
    number
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_digit())
        && number
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        && parts.any(|part| {
            part.chars()
                .any(|character| character.is_ascii_alphabetic())
        })
}

pub(super) fn postal(value: &str) -> Option<String> {
    let value = domestic_location(value);
    value
        .rsplit_once(',')?
        .1
        .split_whitespace()
        .skip(1)
        .find(|token| postal_token(token))
        .map(str::to_owned)
}

fn domestic_location(value: &str) -> &str {
    match value.rsplit_once(',') {
        Some((location, country)) if matches!(country.trim(), "USA" | "US") => location,
        _ => value,
    }
}

fn postal_token(value: &str) -> bool {
    (value.len() == 5 && value.bytes().all(|byte| byte.is_ascii_digit()))
        || (value.len() == 10
            && value.get(5..6) == Some("-")
            && value
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 5 || byte.is_ascii_digit()))
}

pub(super) fn relay(event: &str) -> bool {
    let lower = event.to_ascii_lowercase();
    lower.contains("relay")
        || ["4x", "4 x", "4×", "4 ×", "medley"]
            .iter()
            .any(|marker| lower.contains(marker))
}

pub(super) fn access(document: &Html) -> Result<()> {
    let root = document.root_element();
    let title = value(root, "title")?.map_or_else(String::new, |title| title.to_ascii_lowercase());
    let denied = [
        "access denied",
        "just a moment",
        "attention required",
        "sign in",
        "log in",
        "login",
        "subscribe",
    ]
    .iter()
    .any(|marker| title.contains(marker));
    if denied || root.select(&selector("form[action*='/login'] input[type='password'], #challenge-form, #cf-challenge-running")?).next().is_some() {
        return Err(crate::sources::AccessDenied.into());
    }
    Ok(())
}

pub(super) fn masked(root: ElementRef<'_>) -> Result<bool> {
    let mask = selector(".mask, .redact, .record--locked, .paywall, [aria-label='Locked']")?;
    Ok(mask.matches(&root) || root.select(&mask).next().is_some())
}

pub(super) fn gender(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "m" | "male" | "boys" | "men" => Some("male".to_owned()),
        "f" | "female" | "girls" | "women" => Some("female".to_owned()),
        _ => None,
    }
}

pub(super) fn date(value: &str) -> String {
    ["%b %e, %Y", "%B %e, %Y", "%Y-%m-%d", "%m/%d/%Y"]
        .iter()
        .find_map(|format| chrono::NaiveDate::parse_from_str(value, format).ok())
        .map_or_else(
            || value.to_owned(),
            |date| date.format("%Y-%m-%d").to_string(),
        )
}

pub(super) struct PublishedFile {
    pub identity: String,
    pub unresolved: Option<String>,
}

pub(super) fn published_file(
    target: &crate::model::Target,
    document: &Html,
) -> Result<PublishedFile> {
    let canonical = attribute(document.root_element(), "link[rel=canonical]", "href")?;
    let selected = attribute(
        document.root_element(),
        "#ddResultsPage option[selected][value]",
        "value",
    )?;
    match resolve_published_file(target, canonical.as_deref(), selected.as_deref()) {
        Ok(identity) => Ok(PublishedFile {
            identity,
            unresolved: None,
        }),
        Err(error) => Ok(PublishedFile {
            identity: format!("milesplit-unresolved:{}", target.url),
            unresolved: Some(format!(
                "Unresolved published-file identity: {error}; rows remain scoped to the captured source URL without alias deduplication"
            )),
        }),
    }
}

fn resolve_published_file(
    target: &crate::model::Target,
    canonical: Option<&str>,
    selected: Option<&str>,
) -> Result<String> {
    use super::targets;
    let requested = targets::public_url(&target.url)?;
    let meet = targets::path_id(&target.url, "meets")?;
    let requested_file = result_file(&requested)?;
    let canonical = canonical
        .map(|href| targets::linked(&target.url, href))
        .transpose()?
        .map(|href| targets::public_url(&href))
        .transpose()?;
    let file = if let Some(canonical) = canonical.as_ref() {
        anyhow::ensure!(
            same_result_site(canonical, &requested)
                && targets::path_id(canonical.as_str(), "meets")? == meet,
            "canonical result does not belong to the requested meet and site"
        );
        let file = result_file(canonical)?
            .ok_or_else(|| anyhow::anyhow!("canonical result has no numbered file"))?;
        anyhow::ensure!(
            requested_file.is_none_or(|requested| requested == file),
            "requested and canonical result files conflict"
        );
        file
    } else {
        requested_file.ok_or_else(|| anyhow::anyhow!("no published numbered file evidence"))?
    };
    if let Some(selected) = selected {
        let selected = targets::public_url(&targets::linked(&target.url, selected)?)?;
        anyhow::ensure!(
            same_result_site(&selected, &requested)
                && targets::path_id(selected.as_str(), "meets")? == meet
                && result_file(&selected)?.is_none_or(|selected| selected == file),
            "selected and published result files conflict"
        );
    }
    Ok(format!("milesplit-file:{meet}:{file}"))
}

fn same_result_site(left: &url::Url, right: &url::Url) -> bool {
    left.host_str() == right.host_str()
        || left.host_str() == Some("www.milesplit.com")
        || right.host_str() == Some("www.milesplit.com")
}

fn result_file(url: &url::Url) -> Result<Option<&str>> {
    let mut parts = url.path().trim_end_matches('/').split('/');
    anyhow::ensure!(
        parts.next() == Some("")
            && parts.next() == Some("meets")
            && parts.next().is_some()
            && parts.next() == Some("results"),
        "not a meet result URL"
    );
    let Some(file) = parts.next() else {
        return Ok(None);
    };
    super::targets::id(file)?;
    anyhow::ensure!(
        matches!(parts.next(), None | Some("raw" | "formatted")) && parts.next().is_none(),
        "unrecognized published result file path"
    );
    Ok(Some(file))
}
