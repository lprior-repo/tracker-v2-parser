use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use chrono::DateTime;
use serde::Deserialize;
use url::Url;

use crate::{
    durable::operations::Artifact,
    model::{Gender, Source, Sport},
};

const MAX_MANIFEST_BYTES: usize = 128 * 1024;
const MAX_EVIDENCE: usize = 32;
const MAX_CHANGES: usize = 128;
const MAX_EVIDENCE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) schema_revision: u32,
    pub(super) reviewed_by: String,
    pub(super) researcher: String,
    pub(super) authorization_quote: String,
    pub(super) reviewed_at: String,
    pub(super) evidence: Vec<Evidence>,
    pub(super) changes: Vec<Change>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    pub artifact: Artifact,
    pub primary_url: String,
    pub captured_at: String,
    pub capture_kind: CaptureKind,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CaptureKind {
    WebToolText,
    BrowserDom,
    RetainedHtml,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Change {
    SchoolAddress(SchoolAddress),
    Coach(Coach),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SchoolAddress {
    pub source: Source,
    pub source_id: String,
    pub expected_school_id: i64,
    pub expected_name: String,
    pub expected_city: Option<String>,
    pub expected_state: Option<String>,
    pub address: String,
    pub postal_code: Option<String>,
    pub evidence: Vec<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Coach {
    pub source: Source,
    pub source_id: String,
    pub expected_school_id: i64,
    pub expected_name: String,
    pub expected_city: Option<String>,
    pub expected_state: Option<String>,
    pub name: String,
    pub sport: Sport,
    pub gender: Gender,
    pub role: String,
    pub email: String,
    pub name_role_evidence: Vec<usize>,
    pub email_evidence: Vec<usize>,
}

#[derive(Debug)]
pub struct Reviewed {
    pub(super) manifest: Manifest,
    pub(super) sha256: String,
}

pub(super) fn parse(
    workspace: &super::super::Workspace,
    bytes: &[u8],
    sha256: String,
) -> Result<Reviewed> {
    ensure!(
        bytes.len() <= MAX_MANIFEST_BYTES,
        "research manifest exceeds 128 KiB"
    );
    let manifest: Manifest = serde_json::from_slice(bytes).context("parsing research manifest")?;
    validate(workspace, &manifest)?;
    Ok(Reviewed { manifest, sha256 })
}

fn validate(workspace: &super::super::Workspace, manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.schema_revision == 1
            && manifest.reviewed_by == "codex"
            && manifest.researcher == "gpt-6-luna",
        "research manifest requires schema 1, reviewer codex, and researcher gpt-6-luna"
    );
    validate_timestamp(&manifest.reviewed_at, "review timestamp")?;
    ensure!(
        !manifest.authorization_quote.trim().is_empty()
            && manifest.authorization_quote.len() <= 2048,
        "research manifest requires the bounded explicit user authorization quote"
    );
    ensure!(
        (1..=MAX_EVIDENCE).contains(&manifest.evidence.len())
            && (1..=MAX_CHANGES).contains(&manifest.changes.len()),
        "research manifest evidence or change count is outside bounds"
    );
    let evidence_bytes = manifest
        .evidence
        .iter()
        .try_fold(0_u64, |total, evidence| {
            workspace.read(&evidence.artifact)?;
            validate_url(&evidence.primary_url)?;
            validate_timestamp(&evidence.captured_at, "evidence capture timestamp")?;
            total
                .checked_add(evidence.artifact.bytes)
                .context("evidence byte count overflow")
        })?;
    ensure!(
        evidence_bytes <= MAX_EVIDENCE_BYTES,
        "research evidence exceeds 128 MiB"
    );
    validate_changes(&manifest.changes, &manifest.evidence)
}

fn validate_changes(changes: &[Change], evidence: &[Evidence]) -> Result<()> {
    let targets = changes
        .iter()
        .map(|change| match change {
            Change::SchoolAddress(item) => {
                validate_target(
                    item.source_id.as_str(),
                    item.expected_school_id,
                    &item.expected_name,
                )?;
                ensure!(
                    !item.address.trim().is_empty() && item.address.len() <= 512,
                    "school address is empty or oversized"
                );
                ensure!(
                    item.postal_code
                        .as_deref()
                        .is_none_or(|value| !value.trim().is_empty() && value.len() <= 32),
                    "school postal code is empty or oversized"
                );
                validate_refs(&item.evidence, evidence.len())?;
                Ok(format!("address:{}", item.expected_school_id))
            }
            Change::Coach(item) => {
                validate_target(
                    item.source_id.as_str(),
                    item.expected_school_id,
                    &item.expected_name,
                )?;
                ensure!(
                    !item.name.trim().is_empty() && item.name.len() <= 256,
                    "coach name is empty or oversized"
                );
                ensure!(
                    !item.role.trim().is_empty() && item.role.len() <= 256,
                    "coach role is empty or oversized"
                );
                ensure!(
                    item.email.len() <= 320 && valid_email(&item.email),
                    "coach email is invalid or oversized"
                );
                validate_refs(&item.name_role_evidence, evidence.len())?;
                validate_refs(&item.email_evidence, evidence.len())?;
                let primary_index = item
                    .name_role_evidence
                    .first()
                    .context("coach name/role evidence is required")?;
                let primary_url = evidence
                    .get(*primary_index)
                    .context("validated coach evidence reference disappeared")?
                    .primary_url
                    .as_str();
                Ok(serde_json::to_string(&(
                    item.expected_school_id,
                    primary_url,
                    item.name.as_str(),
                    item.role.as_str(),
                    item.sport.as_str(),
                    item.email.trim().to_ascii_lowercase(),
                ))?)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let unique: BTreeSet<_> = targets.iter().collect();
    ensure!(
        unique.len() == targets.len(),
        "research manifest contains duplicate changes"
    );
    Ok(())
}

fn validate_target(source_id: &str, school_id: i64, name: &str) -> Result<()> {
    ensure!(
        !source_id.trim().is_empty() && source_id.len() <= 128 && school_id > 0,
        "research change requires an exact bounded school source ID and database ID"
    );
    ensure!(
        !name.trim().is_empty() && name.len() <= 256,
        "expected school name is empty or oversized"
    );
    Ok(())
}

fn validate_refs(refs: &[usize], count: usize) -> Result<()> {
    ensure!(
        !refs.is_empty() && refs.iter().all(|index| *index < count),
        "research change has invalid evidence references"
    );
    let unique: BTreeSet<_> = refs.iter().collect();
    ensure!(
        unique.len() == refs.len(),
        "research change repeats an evidence reference"
    );
    Ok(())
}

fn validate_timestamp(value: &str, label: &str) -> Result<()> {
    let parsed = DateTime::parse_from_rfc3339(value).with_context(|| format!("invalid {label}"))?;
    ensure!(
        parsed.offset().local_minus_utc() == 0 && value.ends_with('Z'),
        "{label} must use UTC Z notation"
    );
    Ok(())
}

fn validate_url(value: &str) -> Result<()> {
    let url = Url::parse(value).context("invalid primary evidence URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "evidence URL must be public HTTP(S) without credentials"
    );
    crate::transport::policy::check_url_secrets(&url)
}

fn valid_email(value: &str) -> bool {
    value.split_once('@').is_some_and(|(local, domain)| {
        !local.trim().is_empty()
            && !domain.trim().is_empty()
            && !value.chars().any(char::is_whitespace)
            && domain.contains('.')
    })
}
