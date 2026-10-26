use std::sync::LazyLock;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::de::IgnoredAny;

static JWT: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b"));
static STRING: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#""(?:\\.|[^"\\])*""#));
static SECRET: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?i)token|jwt|authorization|cookie"));

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
    ensure!(
        !secret.is_match(text)
            && !text.contains("\\u")
            && !text.contains("\\x")
            && !text.contains("&#"),
        "cannot safely redact secrets in a non-JSON response"
    );
    Ok(jwt.replace_all(text, "[REDACTED]").as_bytes().to_vec())
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
