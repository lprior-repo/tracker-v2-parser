mod scripts;

use std::sync::LazyLock;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::de::IgnoredAny;

static JWT: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b"));
static STRING: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#""(?:\\.|[^"\\])*""#));
static SECRET: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)token|jwt|authorization|cookie|secret|password|api[_-]?key|access[_-]?key|credential",
    )
});
static ASSIGNMENT: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        (?P<key>"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|
            (?:[A-Za-z0-9_$-]|\\(?:u(?:[A-Za-z0-9]{0,4}|\{[^}\s=;]*\}?)|x[A-Za-z0-9]{0,2})|&\#(?:[xX][0-9a-fA-F]+|[0-9]+);)+)
        (?:\s|/\*[\s\S]*?\*/|//[^\r\n]*(?:\r?\n|$))*
        (?:\](?:\s|/\*[\s\S]*?\*/|//[^\r\n]*(?:\r?\n|$))*)?
        (?P<operator>:|(?:\*\*|<<|>>>|>>|&&|\|\||\?\?|[+\-*/%&|^])?=)
        (?:\s|/\*[\s\S]*?\*/|//[^\r\n]*(?:\r?\n|$))*"#,
    )
});
static SINGLE_QUOTED: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^'(?:\\.|[^'\\])*'"));
static EMPTY_CONSTRUCTOR: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^(?P<expression>new\s+[A-Za-z_$][A-Za-z0-9_$.]*(?:\(\))?)\s*;"));
static FORM: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?is)<(?:input|meta)\b[^>]*>"));
static ENTITY: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"&#(?:[xX][0-9a-fA-F]+|[0-9]+);"));
static IDENTIFIER_ESCAPE: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"\\(?:u(?:\{(?P<braced>[0-9a-fA-F]+)\}|(?P<fixed>[0-9a-fA-F]{4}))|x(?P<byte>[0-9a-fA-F]{2}))",
    )
});

pub(super) fn jwt_pattern() -> Result<&'static Regex> {
    JWT.as_ref().context("compiling token redaction")
}

pub(super) fn secret_key_pattern() -> Result<&'static Regex> {
    SECRET.as_ref().context("compiling secret redaction")
}

pub(super) fn body(raw: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        raw.len() <= 16 * 1024 * 1024,
        "source response exceeds 16 MiB"
    );
    let text = std::str::from_utf8(raw).context("source response is not UTF-8")?;
    let jwt = jwt_pattern()?;
    let strings = STRING.as_ref().context("compiling JSON string matcher")?;
    let secret = secret_key_pattern()?;
    if serde_json::from_str::<IgnoredAny>(text).is_ok() {
        return json(text, strings, secret, jwt);
    }
    let trimmed = text.trim_start();
    ensure!(
        !trimmed.starts_with('{') && !trimmed.starts_with('['),
        "cannot safely redact malformed JSON"
    );
    html(text, secret, jwt)
}

fn html(text: &str, secret: &Regex, jwt: &Regex) -> Result<Vec<u8>> {
    let ranges = scripts::unsupported(text, secret, jwt)?;
    if ranges.is_empty() {
        return html_assignments(text, secret, jwt);
    }
    let (mut sanitized, end) = ranges.into_iter().try_fold(
        (String::with_capacity(text.len()), 0),
        |(mut output, end), range| -> Result<(String, usize)> {
            ensure!(range.start >= end, "overlapping sensitive script ranges");
            output.push_str(
                text.get(end..range.start)
                    .context("invalid script boundary")?,
            );
            Ok((output, range.end))
        },
    )?;
    sanitized.push_str(text.get(end..).context("invalid script boundary")?);
    html_assignments(&sanitized, secret, jwt)
}

fn html_assignments(text: &str, secret: &Regex, jwt: &Regex) -> Result<Vec<u8>> {
    let assignments = ASSIGNMENT
        .as_ref()
        .context("compiling assignment matcher")?;
    let single = SINGLE_QUOTED
        .as_ref()
        .context("compiling quoted value matcher")?;
    let forms = FORM.as_ref().context("compiling sensitive form matcher")?;
    let entities = ENTITY.as_ref().context("compiling entity matcher")?;
    ensure!(
        !scripts::ambiguous_computed(text),
        "cannot safely redact computed property assignments"
    );
    ensure!(
        !forms
            .find_iter(text)
            .any(|tag| { secret.is_match(&decode_entities(tag.as_str(), entities)) }),
        "cannot safely redact credential form fields"
    );
    let (mut output, end) = assignments.captures_iter(text).try_fold(
        (String::with_capacity(text.len()), 0),
        |(mut output, end), captures| -> Result<(String, usize)> {
            let assignment = captures.get(0).context("missing assignment")?;
            if assignment.start() < end {
                return Ok((output, end));
            }
            let key = captures
                .name("key")
                .context("missing assignment key")?
                .as_str();
            if !secret.is_match(key)
                && !key.contains("\\u")
                && !key.contains("\\x")
                && !key.contains("&#")
            {
                return Ok((output, end));
            }
            let key = decode_key(key, entities)?;
            if !secret.is_match(&key) {
                return Ok((output, end));
            }
            ensure!(
                key.chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '_' | '$' | '-')),
                "cannot safely redact ambiguous sensitive assignment key"
            );
            let value = text
                .get(assignment.end()..)
                .context("invalid assignment boundary")?;
            if value.starts_with('=') {
                return Ok((output, end));
            }
            let length = value_length(value, single)?;
            ensure!(
                captures
                    .name("operator")
                    .is_some_and(|operator| { matches!(operator.as_str(), ":" | "=") }),
                "cannot safely redact compound sensitive assignment"
            );
            let remainder = value
                .get(length..)
                .context("invalid sensitive expression boundary")?
                .trim_start();
            ensure!(
                remainder.is_empty()
                    || remainder.starts_with([';', ',', '}', ']'])
                    || remainder.starts_with("</script"),
                "cannot safely redact unsupported sensitive expression"
            );
            let value_end = assignment
                .end()
                .checked_add(length)
                .context("invalid assignment value offset")?;
            output.push_str(
                text.get(end..assignment.end())
                    .context("invalid assignment boundary")?,
            );
            output.push_str("\"[REDACTED]\"");
            Ok((output, value_end))
        },
    )?;
    output.push_str(text.get(end..).context("invalid response boundary")?);
    Ok(jwt.replace_all(&output, "[REDACTED]").as_bytes().to_vec())
}

fn decode_key(key: &str, entities: &Regex) -> Result<String> {
    let key = if key.starts_with('"') {
        serde_json::from_str::<String>(key).context("invalid quoted assignment key")?
    } else {
        decode_identifier(key.trim_matches('\''))?
    };
    Ok(decode_entities(&key, entities))
}

fn decode_identifier(key: &str) -> Result<String> {
    let escapes = IDENTIFIER_ESCAPE
        .as_ref()
        .context("compiling identifier escapes")?;
    let (mut decoded, end) = escapes.captures_iter(key).try_fold(
        (String::with_capacity(key.len()), 0),
        |(mut decoded, end), capture| -> Result<_> {
            let matched = capture.get(0).context("missing identifier escape")?;
            let character = escaped_character(&capture)?;
            decoded.push_str(
                key.get(end..matched.start())
                    .context("invalid identifier boundary")?,
            );
            decoded.push(character);
            Ok((decoded, matched.end()))
        },
    )?;
    decoded.push_str(key.get(end..).context("invalid identifier suffix")?);
    ensure!(
        !decoded.contains('\\'),
        "cannot safely decode assignment key"
    );
    Ok(decoded)
}

fn escaped_character(capture: &regex::Captures<'_>) -> Result<char> {
    let hex = capture
        .name("braced")
        .or_else(|| capture.name("fixed"))
        .or_else(|| capture.name("byte"))
        .context("missing identifier codepoint")?;
    let significant = hex.as_str().trim_start_matches('0');
    ensure!(significant.len() <= 6, "invalid identifier codepoint");
    let scalar = if significant.is_empty() {
        0
    } else {
        u32::from_str_radix(significant, 16)?
    };
    char::from_u32(scalar).context("invalid identifier codepoint")
}

fn decode_entities(text: &str, entities: &Regex) -> String {
    entities
        .replace_all(text, |capture: &regex::Captures<'_>| {
            let entity = capture.get(0).map_or("", |value| value.as_str());
            let digits = entity.trim_start_matches("&#").trim_end_matches(';');
            let (digits, radix) = digits
                .strip_prefix('x')
                .or_else(|| digits.strip_prefix('X'))
                .map_or((digits, 10), |hex| (hex, 16));
            u32::from_str_radix(digits, radix)
                .ok()
                .and_then(char::from_u32)
                .map_or_else(|| entity.to_owned(), |character| character.to_string())
        })
        .into_owned()
}

fn value_length(value: &str, single: &Regex) -> Result<usize> {
    if value.starts_with('\'') {
        return single
            .find(value)
            .map(|matched| matched.end())
            .context("cannot safely redact malformed quoted value");
    }
    if let Some(literal) = ["null", "true", "false"].into_iter().find(|literal| {
        value
            .strip_prefix(literal)
            .is_some_and(|rest| rest.starts_with(';'))
    }) {
        return Ok(literal.len());
    }
    let constructor = EMPTY_CONSTRUCTOR
        .as_ref()
        .context("compiling constructor matcher")?;
    if let Some(expression) = constructor
        .captures(value)
        .and_then(|capture| capture.name("expression"))
    {
        return Ok(expression.end());
    }
    let mut stream = serde_json::Deserializer::from_str(value).into_iter::<IgnoredAny>();
    stream
        .next()
        .context("missing sensitive assignment value")?
        .context("cannot safely redact sensitive assignment value")?;
    Ok(stream.byte_offset())
}

fn json(text: &str, strings: &Regex, secret: &Regex, jwt: &Regex) -> Result<Vec<u8>> {
    let (mut output, end) = strings.find_iter(text).try_fold(
        (String::with_capacity(text.len()), 0),
        |(mut output, end), token| -> Result<(String, usize)> {
            if token.start() < end {
                return Ok((output, end));
            }
            let decoded: String = serde_json::from_str(token.as_str())?;
            let suffix = text
                .get(token.end()..)
                .context("invalid JSON string boundary")?;
            let after = suffix.trim_start();
            let is_key = after.starts_with(':');
            if is_key && secret.is_match(&decoded) {
                let value = after.get(1..).context("missing JSON value")?;
                let value_start = text
                    .len()
                    .checked_sub(value.len())
                    .context("invalid JSON value offset")?;
                let mut stream =
                    serde_json::Deserializer::from_str(value).into_iter::<IgnoredAny>();
                stream.next().context("missing JSON value")??;
                let value_end = value_start
                    .checked_add(stream.byte_offset())
                    .context("invalid JSON value offset")?;
                output.push_str(
                    text.get(end..value_start)
                        .context("invalid JSON value boundary")?,
                );
                output.push_str(" \"[REDACTED]\"");
                return Ok((output, value_end));
            }
            let redacted = jwt.replace_all(&decoded, "[REDACTED]");
            if redacted == decoded {
                return Ok((output, end));
            }
            output.push_str(
                text.get(end..token.start())
                    .context("invalid JSON token boundary")?,
            );
            output.push_str(&serde_json::to_string(&redacted)?);
            Ok((output, token.end()))
        },
    )?;
    output.push_str(text.get(end..).context("invalid JSON end boundary")?);
    Ok(output.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_redacted_without_changing_public_result_marks() -> Result<()> {
        let raw = br#"{"jwtTeamHome":"eyJhbGciOiJIUzI1NiJ9.eyJ0ZWFtIjozMjA0fQ.signature","Result":"44.88","token":"private-value"}"#;
        let value: serde_json::Value = serde_json::from_slice(&body(raw)?)?;
        assert_eq!(value["jwtTeamHome"], "[REDACTED]");
        assert_eq!(value["token"], "[REDACTED]");
        assert_eq!(value["Result"], "44.88");
        Ok(())
    }
}
