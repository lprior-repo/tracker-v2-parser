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
