use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use url::{Host, Url};

use super::{Change, Evidence, validate_refs, validate_target};
use crate::{durable::operations::Workspace, model::Source};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchoolWebsite {
    pub source: Source,
    pub source_id: String,
    pub expected_school_id: i64,
    pub expected_name: String,
    pub expected_city: Option<String>,
    pub expected_state: Option<String>,
    pub website: String,
    pub contact_url: String,
    pub season: i32,
    pub evidence: Vec<usize>,
}

pub(super) fn validate(item: &SchoolWebsite, evidence_count: usize) -> Result<()> {
    validate_target(
        &item.source_id,
        item.expected_school_id,
        &item.expected_name,
    )?;
    validate_refs(&item.evidence, evidence_count)?;
    ensure!(
        (1900..=2200).contains(&item.season),
        "invalid contact season"
    );
    let website = public_https(&item.website)?;
    let contact = public_https(&item.contact_url)?;
    ensure!(
        website.origin() == contact.origin(),
        "reviewed contact URL must share the published school website origin"
    );
    Ok(())
}

fn public_https(value: &str) -> Result<Url> {
    ensure!(
        value.len() <= 2048
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
            && !value.contains('\\'),
        "reviewed school URL is oversized or ambiguous"
    );
    let url = Url::parse(value).context("invalid reviewed school URL")?;
    let Some(Host::Domain(domain)) = url.host() else {
        anyhow::bail!("reviewed school URL requires a public DNS hostname");
    };
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url.fragment().is_none(),
        "reviewed school URL requires credential-free HTTPS without fragment"
    );
    ensure!(
        domain.contains('.')
            && !domain.ends_with('.')
            && ![
                "localhost",
                "local",
                "internal",
                "home",
                "lan",
                "invalid",
                "test"
            ]
            .iter()
            .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}"))),
        "reviewed school URL requires a public DNS hostname"
    );
    crate::transport::policy::check_url_secrets(&url)?;
    Ok(url)
}

pub(super) fn validate_evidence(
    workspace: &Workspace,
    changes: &[Change],
    evidence: &[Evidence],
) -> Result<()> {
    changes.iter().try_for_each(|change| {
        let Change::SchoolWebsite(item) = change else {
            return Ok(());
        };
        let supported = item.evidence.iter().try_fold(false, |supported, index| {
            let evidence = evidence
                .get(*index)
                .context("validated website evidence disappeared")?;
            let bytes = workspace.read(&evidence.artifact)?;
            let text = std::str::from_utf8(&bytes)
                .context("website evidence must be retained UTF-8 text")?;
            let primary = public_https(&evidence.primary_url)?;
            ensure!(
                primary.host().is_some(),
                "website citation has no public host"
            );
            Ok::<_, anyhow::Error>(
                supported
                    || (text.contains(&item.expected_name)
                        && contains_url(text, &item.website)?
                        && contains_url(text, &item.contact_url)?),
            )
        })?;
        ensure!(
            supported,
            "website evidence must publish the exact school name, website and contact URL together"
        );
        Ok(())
    })
}

fn contains_url(text: &str, expected: &str) -> Result<bool> {
    let urls =
        regex::Regex::new(r#"https://[^\s\"'<>]+"#).context("invalid published URL expression")?;
    Ok(urls
        .find_iter(text)
        .any(|found| found.as_str().trim_end_matches([')', ']', ',', ';']) == expected))
}
