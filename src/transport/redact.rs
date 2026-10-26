use std::sync::LazyLock;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::de::IgnoredAny;

static JWT: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b"));
static STRING: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#""(?:\\.|[^"\\])*""#));
static SECRET: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?i)token|jwt|authorization|cookie|secret|password"));
static ASSIGNMENT: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r#"(?P<key>"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|(?:[A-Za-z0-9_$-]|\\(?:u[0-9a-fA-F]{4}|x[0-9a-fA-F]{2})|&#(?:[xX][0-9a-fA-F]+|[0-9]+);)+)\s*[:=]\s*"#,
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

pub(super) fn body(raw: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(raw).context("source response is not UTF-8")?;
    let jwt = JWT.as_ref().context("compiling token redaction")?;
    let strings = STRING.as_ref().context("compiling JSON string matcher")?;
    let secret = SECRET.as_ref().context("compiling secret redaction")?;
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
    let assignments = ASSIGNMENT
        .as_ref()
        .context("compiling assignment matcher")?;
    let single = SINGLE_QUOTED
        .as_ref()
        .context("compiling quoted value matcher")?;
    let forms = FORM.as_ref().context("compiling sensitive form matcher")?;
    let entities = ENTITY.as_ref().context("compiling entity matcher")?;
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
            let value = text
                .get(assignment.end()..)
                .context("invalid assignment boundary")?;
            if value.starts_with('=') {
                return Ok((output, end));
            }
            let length = value_length(value, single)?;
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
        ensure!(!key.contains('\\'), "cannot safely decode assignment key");
        key.trim_matches('\'').to_owned()
    };
    Ok(decode_entities(&key, entities))
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
