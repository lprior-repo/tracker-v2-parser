use anyhow::Result;
use regex::Regex;

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
