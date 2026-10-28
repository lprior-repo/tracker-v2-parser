use anyhow::Result;
use regex::Regex;
use scraper::ElementRef;

pub(super) fn hidden(element: ElementRef<'_>) -> bool {
    hidden_including_script(element, true)
}

pub(super) fn hidden_publication(element: ElementRef<'_>) -> bool {
    hidden_including_script(element, false)
}

fn hidden_including_script(element: ElementRef<'_>, exclude_script: bool) -> bool {
    std::iter::once(element)
        .chain(element.ancestors().filter_map(ElementRef::wrap))
        .any(|parent| {
            matches!(parent.value().name(), "style" | "noscript" | "template")
                || (exclude_script && parent.value().name() == "script")
                || parent.value().attr("hidden").is_some()
                || parent
                    .value()
                    .attr("aria-hidden")
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
                || parent.value().attr("style").is_some_and(hidden_style)
        })
}

fn hidden_style(style: &str) -> bool {
    style.split(';').any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let value = value
            .split_once('!')
            .map_or(value, |(value, _)| value)
            .trim();
        (property.trim().eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
            || (property.trim().eq_ignore_ascii_case("visibility")
                && (value.eq_ignore_ascii_case("hidden") || value.eq_ignore_ascii_case("collapse")))
    })
}

pub(super) fn pattern() -> Result<Regex> {
    Ok(Regex::new(
        r"(?i)^[a-z0-9][a-z0-9._%+\-]*@[a-z0-9](?:[a-z0-9\-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9\-]*[a-z0-9])?)+$",
    )?)
}

pub(super) fn normalize(value: &str, pattern: &Regex) -> Option<String> {
    let value = value.trim_matches(|character: char| {
        matches!(character, '<' | '>' | '(' | ')' | ',' | ';' | '.') || character.is_whitespace()
    });
    (value.len() <= 254 && pattern.is_match(value)).then(|| value.to_ascii_lowercase())
}

pub(super) fn decode_cfemail(encoded: &str, pattern: &Regex) -> Option<String> {
    let hex = encoded.trim();
    if !(4..=510).contains(&hex.len())
        || !hex.len().is_multiple_of(2)
        || !hex.bytes().all(|digit| digit.is_ascii_hexdigit())
    {
        return None;
    }
    let key = u8::from_str_radix(hex.get(..2)?, 16).ok()?;
    let decoded = hex
        .get(2..)?
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
            Some(byte ^ key)
        })
        .collect::<Option<Vec<_>>>()?;
    let address = String::from_utf8(decoded).ok()?;
    normalize(&address, pattern)
}
