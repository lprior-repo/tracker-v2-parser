use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::model::{Performance, Source, Target};

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn serialized<T: Serialize>(value: &T) -> Result<String> {
    Ok(digest(&serde_json::to_vec(value)?))
}

pub(super) fn target(target: &Target) -> Result<String> {
    let method = target
        .method
        .as_deref()
        .map_or("GET", str::trim)
        .to_ascii_uppercase();
    serialized(&(
        target.source,
        target.kind,
        &target.url,
        method,
        &target.body,
        &target.context,
    ))
}

pub(super) fn performance(
    source: Source,
    performance: &Performance,
    url: &str,
    row: usize,
) -> Result<String> {
    if let Some(result_id) = performance
        .result_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        return serialized(&(source, "result", result_id));
    }
    if let Some(source_row) = performance.source_row.as_deref() {
        return serialized(&(source, "source_row", url, source_row));
    }
    serialized(&(source, "page_row_content", url, row, performance))
}

pub(super) fn profile_link(value: &str) -> Result<(Source, String)> {
    let url = Url::parse(value)
        .with_context(|| format!("invalid explicit same_as profile URL: {value}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("same_as profile must be an HTTP(S) URL: {value}");
    }
    let host = url.host_str().context("same_as profile URL has no host")?;
    let parts = url
        .path_segments()
        .context("same_as profile URL has no path")?
        .collect::<Vec<_>>();
    let host_is = |domain: &str| {
        host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    };
    let candidate = if host_is("athletic.net") {
        athletic_id(&url, &parts).map(|id| (Source::AthleticNet, id))
    } else if host_is("milesplit.com") {
        parts
            .windows(2)
            .find(|pair| pair.first() == Some(&"athletes"))
            .and_then(|pair| pair.get(1))
            .and_then(|part| match part.split_once('-') {
                Some((id, slug)) if !slug.is_empty() && !slug.contains('%') => Some(id),
                Some(_) => None,
                None => Some(*part),
            })
            .map(|id| (Source::MileSplit, id.to_owned()))
    } else {
        None
    };
    match candidate {
        Some((source, id)) if !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) => {
            Ok((source, id))
        }
        _ => bail!("same_as must identify an Athletic.net or MileSplit athlete profile: {value}"),
    }
}

fn athletic_id(url: &Url, parts: &[&str]) -> Option<String> {
    parts
        .windows(2)
        .find(|pair| {
            pair.first()
                .is_some_and(|part| part.eq_ignore_ascii_case("athlete"))
        })
        .and_then(|pair| pair.get(1))
        .map(|id| (*id).to_owned())
        .or_else(|| {
            parts
                .last()
                .filter(|part| part.eq_ignore_ascii_case("Athlete.aspx"))?;
            url.query_pairs()
                .find(|(key, _)| key.eq_ignore_ascii_case("AID"))
                .map(|(_, id)| id.into_owned())
        })
}
