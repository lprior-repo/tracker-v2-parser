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
    Ok(root
        .select(&selector(
            ".mask, .redact, .record--locked, .paywall, [aria-label='Locked']",
        )?)
        .next()
        .is_some())
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
