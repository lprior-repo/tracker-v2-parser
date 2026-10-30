use std::ops::Range;

use anyhow::{Context, Result, ensure};
use lol_html::{HtmlRewriter, MemorySettings, Settings, element, html_content::TextChunk, text};
use regex::Regex;

use super::{ASSIGNMENT, ENTITY, decode_key, html_assignments};

pub(super) fn unsupported(text: &str, secret: &Regex, jwt: &Regex) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut pending = None;
    let mut count = 0_usize;
    let settings = Settings::new()
        .with_memory_settings(
            MemorySettings::new()
                .with_max_allowed_memory_usage(1024 * 1024)
                .with_graceful_bail_out_on_memory_limit_exceeded(false),
        )
        .with_graceful_bail_out_on_content_handler_error(false)
        .append_element_content_handler(element!("script", |_| {
            count = count
                .checked_add(1)
                .filter(|count| *count <= 4096)
                .context("source exceeds 4096 script elements")?;
            Ok(())
        }))
        .append_element_content_handler(text!("script", |chunk| {
            inspect(text, chunk, secret, jwt, &mut pending, &mut ranges)?;
            Ok(())
        }));
    let mut parser = HtmlRewriter::new(settings, |_: &[u8]| {});
    parser
        .write(text.as_bytes())
        .context("scanning source script boundaries")?;
    parser.end().context("finishing source script boundaries")?;
    ensure!(pending.is_none(), "incomplete source script text");
    Ok(ranges)
}

fn inspect(
    text: &str,
    chunk: &TextChunk<'_>,
    secret: &Regex,
    jwt: &Regex,
    pending: &mut Option<Range<usize>>,
    ranges: &mut Vec<Range<usize>>,
) -> Result<()> {
    let current = chunk.source_location().bytes();
    let start = pending.as_ref().map_or(current.start, |range| range.start);
    *pending = Some(start..current.end);
    if !chunk.last_in_text_node() {
        return Ok(());
    }
    let range = pending.take().context("missing source script text")?;
    let body = text
        .get(range.clone())
        .context("invalid native script boundary")?;
    if discarded_telemetry_loader(body) {
        ranges.push(range);
        return Ok(());
    }
    if html_assignments(body, secret, jwt).is_ok() {
        return Ok(());
    }
    ensure!(
        !ambiguous_computed(body),
        "cannot safely classify computed script assignments"
    );
    ensure!(
        identified(body, secret)?,
        "cannot safely classify sensitive script"
    );
    ranges.push(range);
    Ok(())
}

fn discarded_telemetry_loader(body: &str) -> bool {
    let Some((configuration, loader)) = body.trim().split_once(
        "/*! For license information please see nr-loader-spa-1.324.0.min.js.LICENSE.txt */",
    ) else {
        return false;
    };
    let loader = loader.trim();
    configuration.starts_with("(window.NREUM||(NREUM={})).init=")
        && configuration.contains(";(window.NREUM||(NREUM={})).loader_config=")
        && loader.starts_with("(()=>{")
        && loader.ends_with("})()})();")
}

fn identified(body: &str, secret: &Regex) -> Result<bool> {
    let assignments = ASSIGNMENT
        .as_ref()
        .context("compiling assignment matcher")?;
    let entities = ENTITY.as_ref().context("compiling entity matcher")?;
    assignments
        .captures_iter(body)
        .try_fold(false, |found, capture| {
            let key = capture
                .name("key")
                .context("missing script assignment key")?
                .as_str();
            if found
                || (!secret.is_match(key)
                    && !key.contains("\\u")
                    && !key.contains("\\x")
                    && !key.contains("&#"))
            {
                return Ok(found);
            }
            Ok(secret.is_match(&decode_key(key, entities)?))
        })
}

pub(super) fn ambiguous_computed(text: &str) -> bool {
    let mut inspected = 0_usize;
    text.match_indices('[').any(|(offset, _)| {
        let Some(suffix) = text
            .get(offset..)
            .and_then(|suffix| suffix.strip_prefix('['))
        else {
            return false;
        };
        let (ambiguous, length) = computed_prefix(suffix);
        inspected = inspected.saturating_add(length);
        ambiguous || inspected > 64 * 1024 * 1024
    })
}

fn computed_prefix(text: &str) -> (bool, usize) {
    let Some((prefix, grouped)) = skip_grouping(text, '(') else {
        return (false, text.len());
    };
    let Some(delimiter @ ('\'' | '"' | '`')) = prefix.chars().next() else {
        return (false, text.len().saturating_sub(prefix.len()));
    };
    let Some(literal_end) = quoted_end(prefix, delimiter) else {
        return (false, text.len());
    };
    let Some((suffix, _)) = prefix
        .get(literal_end..)
        .and_then(|suffix| skip_grouping(suffix, ')'))
    else {
        return (false, text.len());
    };
    let inspected = text.len().saturating_sub(suffix.len());
    if suffix.starts_with('+') {
        return (true, inspected);
    }
    if !grouped && delimiter != '`' {
        return (false, inspected);
    }
    let Some(after_bracket) = suffix.strip_prefix(']') else {
        return (false, inspected);
    };
    let Some(remainder) = skip_trivia(after_bracket) else {
        return (false, text.len());
    };
    (
        assignment_operator(remainder),
        text.len().saturating_sub(remainder.len()),
    )
}

fn skip_grouping(text: &str, delimiter: char) -> Option<(&str, bool)> {
    match (0..=text.len()).try_fold((text, false), |(suffix, grouped), _| {
        let Some(suffix) = skip_trivia(suffix) else {
            return std::ops::ControlFlow::Break(None);
        };
        suffix.strip_prefix(delimiter).map_or(
            std::ops::ControlFlow::Break(Some((suffix, grouped))),
            |suffix| std::ops::ControlFlow::Continue((suffix, true)),
        )
    }) {
        std::ops::ControlFlow::Break(result) => result,
        std::ops::ControlFlow::Continue(_) => None,
    }
}

fn skip_trivia(text: &str) -> Option<&str> {
    match (0..=text.len()).try_fold(text, |suffix, _| {
        let suffix = suffix.trim_start();
        let remainder = if let Some(comment) = suffix.strip_prefix("/*") {
            comment.split_once("*/").map(|(_, remainder)| remainder)
        } else if let Some(comment) = suffix.strip_prefix("//") {
            Some(
                comment
                    .split_once(['\r', '\n', '\u{2028}', '\u{2029}'])
                    .map_or("", |(_, remainder)| remainder),
            )
        } else {
            return std::ops::ControlFlow::Break(Some(suffix));
        };
        remainder.map_or(
            std::ops::ControlFlow::Break(None),
            std::ops::ControlFlow::Continue,
        )
    }) {
        std::ops::ControlFlow::Break(result) => result,
        std::ops::ControlFlow::Continue(_) => None,
    }
}

fn quoted_end(text: &str, delimiter: char) -> Option<usize> {
    let mut escaped = false;
    text.char_indices()
        .skip(1)
        .find(|(_, character)| {
            if escaped {
                escaped = false;
                return false;
            }
            if *character == '\\' {
                escaped = true;
                return false;
            }
            *character == delimiter
        })
        .and_then(|(offset, _)| offset.checked_add(delimiter.len_utf8()))
}

fn assignment_operator(text: &str) -> bool {
    !text.starts_with("==")
        && !text.starts_with("=>")
        && [
            "=", "+=", "-=", "*=", "/=", "%=", "**=", "<<=", ">>=", ">>>=", "&=", "|=", "^=",
            "&&=", "||=", "??=",
        ]
        .into_iter()
        .any(|operator| text.starts_with(operator))
}

#[cfg(test)]
#[path = "scripts_tests.rs"]
mod tests;
