use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::{
    durable::operations::{Artifact, Workspace},
    model::{PageContext, PageKind, Source, Sport, Target},
};

const MAX_MANIFEST_BYTES: usize = 128 * 1024;
const MAX_EVIDENCE: usize = 32;
const MAX_LINKS: usize = 64;
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
    pub(super) changes: Vec<ResultLink>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    pub(super) artifact: Artifact,
    pub(super) primary_url: String,
    pub(super) captured_at: String,
    pub(super) capture_kind: CaptureKind,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CaptureKind {
    WebToolText,
    BrowserDom,
    RetainedHtml,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResultLink {
    pub(super) source: Source,
    pub(super) source_result_key: String,
    pub(super) source_result_id: String,
    pub(super) source_url: String,
    pub(super) event: String,
    pub(super) mark: String,
    pub(super) sport: Sport,
    pub(super) meet_id: String,
    pub(super) meet_date: String,
    pub(super) status: String,
    pub(super) represented_school_id: Option<i64>,
    pub(super) raw_name: String,
    pub(super) raw_team: String,
    pub(super) raw_grade: String,
    pub(super) published_file_identity: String,
    pub(super) line_ordinal: i64,
    pub(super) published_html: String,
    pub(super) target_source_id: String,
    pub(super) target_athlete_source_id: i64,
    pub(super) target_athlete_id: i64,
    pub(super) target_profile_url: String,
    pub(super) target_name: String,
    pub(super) target_gender: Option<String>,
    pub(super) target_graduation_year: i32,
    pub(super) target_school_id: i64,
    pub(super) target_school_name: String,
    pub(super) target_school_state: String,
    pub(super) result_evidence: Vec<usize>,
    pub(super) identity_evidence: Vec<usize>,
}

#[derive(Debug)]
pub struct Prepared {
    pub(super) manifest: Manifest,
    pub(super) sha256: String,
}

pub(super) fn prepare(workspace: &Workspace, artifact: &Artifact) -> Result<Prepared> {
    let bytes = workspace.read(artifact)?;
    ensure!(
        bytes.len() <= MAX_MANIFEST_BYTES,
        "reviewed result-link manifest exceeds 128 KiB"
    );
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("parsing reviewed result-link manifest")?;
    validate(workspace, &manifest)?;
    Ok(Prepared {
        manifest,
        sha256: artifact.sha256.to_ascii_lowercase(),
    })
}

fn validate(workspace: &Workspace, manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.schema_revision == 1
            && manifest.reviewed_by == "codex"
            && manifest.researcher == "gpt-6-luna",
        "reviewed result-link manifest requires schema 1, reviewer codex, and researcher gpt-6-luna"
    );
    validate_timestamp(&manifest.reviewed_at, "review timestamp")?;
    ensure!(
        !manifest.authorization_quote.trim().is_empty()
            && manifest.authorization_quote.len() <= 2048,
        "reviewed result-link manifest requires the bounded user-authorization quote"
    );
    ensure!(
        (1..=MAX_EVIDENCE).contains(&manifest.evidence.len())
            && (1..=MAX_LINKS).contains(&manifest.changes.len()),
        "reviewed result-link evidence or change count is outside bounds"
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
                .context("reviewed result-link evidence byte count overflow")
        })?;
    ensure!(
        evidence_bytes <= MAX_EVIDENCE_BYTES,
        "reviewed result-link evidence exceeds 128 MiB"
    );
    validate_links(&manifest.changes, &manifest.evidence)
}

fn validate_links(changes: &[ResultLink], evidence: &[Evidence]) -> Result<()> {
    let keys = changes
        .iter()
        .map(|change| {
            validate_link(change, evidence)?;
            Ok(change.source_result_key.as_str())
        })
        .collect::<Result<Vec<_>>>()?;
    let unique: BTreeSet<_> = keys.iter().collect();
    ensure!(
        unique.len() == keys.len(),
        "reviewed result-link manifest repeats a result key"
    );
    Ok(())
}

fn validate_link(change: &ResultLink, evidence: &[Evidence]) -> Result<()> {
    ensure!(
        change.source == Source::MileSplit && change.sport == Sport::CrossCountry,
        "reviewed result links currently require MileSplit cross-country rows"
    );
    ensure!(
        is_sha256(&change.source_result_key)
            && !change.source_result_id.trim().is_empty()
            && change.source_result_id.len() <= 4096,
        "reviewed result link requires a 64-digit result key and bounded source result ID"
    );
    ensure!(
        !change.event.trim().is_empty()
            && change.event.len() <= 256
            && !change.mark.trim().is_empty()
            && change.mark.len() <= 128
            && change.meet_id.bytes().all(|byte| byte.is_ascii_digit())
            && change.meet_date.len() <= 32
            && change.status == "recorded"
            && change.represented_school_id.is_none_or(|id| id > 0),
        "reviewed result link has invalid result facts"
    );
    ensure!(
        change.target_athlete_source_id > 0
            && change.target_athlete_id > 0
            && change.target_school_id > 0
            && change.target_graduation_year >= 1900
            && change.target_graduation_year <= 2200
            && !change.target_name.trim().is_empty()
            && !change.target_school_name.trim().is_empty()
            && change.target_school_state.len() == 2,
        "reviewed result link has invalid target facts"
    );
    validate_result_url(change)?;
    validate_profile_url(change)?;
    validate_raw_witness(change)?;
    validate_refs(&change.result_evidence, evidence.len())?;
    validate_refs(&change.identity_evidence, evidence.len())?;
    ensure!(
        change.result_evidence.iter().any(|index| evidence
            .get(*index)
            .is_some_and(|item| item.primary_url == change.source_url))
            && change.identity_evidence.iter().any(|index| evidence
                .get(*index)
                .is_some_and(|item| item.primary_url == change.target_profile_url)),
        "reviewed result link requires exact result-page and target-profile evidence URLs"
    );
    Ok(())
}

fn validate_result_url(change: &ResultLink) -> Result<()> {
    let url = Url::parse(&change.source_url).context("invalid reviewed result URL")?;
    let host = url.host_str().context("reviewed result URL has no host")?;
    let host_ok = host == "milesplit.com" || host.ends_with(".milesplit.com");
    let parts = url
        .path_segments()
        .context("reviewed result URL has no path")?
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let meet_matches = parts
        .get(1)
        .and_then(|slug| slug.split('-').next())
        .is_some_and(|meet| meet == change.meet_id);
    let file_matches = parts.get(3).is_some_and(|file| {
        *file == source_file_id(&change.published_file_identity).unwrap_or_default()
    });
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && host_ok
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && parts.len() == 5
            && parts.first() == Some(&"meets")
            && parts.get(2) == Some(&"results")
            && matches!(parts.get(4), Some(&"raw" | &"formatted"))
            && meet_matches
            && file_matches,
        "reviewed result URL does not identify the expected public MileSplit result file"
    );
    crate::transport::policy::check_url_secrets(&url)
}

fn source_file_id(identity: &str) -> Option<&str> {
    let file = identity.strip_prefix("milesplit-file:")?.split_once(':')?.1;
    (!file.is_empty() && file.bytes().all(|byte| byte.is_ascii_digit())).then_some(file)
}

fn validate_profile_url(change: &ResultLink) -> Result<()> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        change.target_profile_url.clone(),
        PageContext::default(),
    );
    crate::transport::validate_target(&target)?;
    ensure!(
        crate::sources::milesplit::profile_identity(&change.target_profile_url)?
            == change.target_source_id,
        "reviewed target profile URL does not match the exact target source ID"
    );
    Ok(())
}

fn validate_raw_witness(change: &ResultLink) -> Result<()> {
    let prefix = change
        .source_result_id
        .strip_prefix("raw:")
        .context("reviewed source result ID must preserve raw published coordinates")?;
    let coordinates: Vec<Value> =
        serde_json::from_str(prefix).context("parsing reviewed raw result coordinates")?;
    ensure!(
        coordinates.len() == 6
            && coordinates.first().and_then(Value::as_str) == Some(change.meet_id.as_str())
            && coordinates.get(1).and_then(Value::as_str) == Some(&change.published_file_identity)
            && coordinates.get(2).and_then(Value::as_str) == Some(&change.event)
            && coordinates.get(4).and_then(Value::as_i64) == Some(change.line_ordinal)
            && coordinates
                .get(5)
                .and_then(Value::as_str)
                .is_some_and(is_sha256)
            && change
                .published_file_identity
                .starts_with("milesplit-file:")
            && change.line_ordinal > 0
            && !change.raw_name.trim().is_empty()
            && !change.raw_team.trim().is_empty()
            && !change.raw_grade.trim().is_empty()
            && !change.published_html.trim().is_empty(),
        "reviewed result link has inconsistent raw row evidence"
    );
    Ok(())
}

fn validate_refs(references: &[usize], count: usize) -> Result<()> {
    ensure!(
        !references.is_empty() && references.iter().all(|index| *index < count),
        "reviewed result link has invalid evidence references"
    );
    let unique: BTreeSet<_> = references.iter().collect();
    ensure!(
        unique.len() == references.len(),
        "reviewed result link repeats an evidence reference"
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
    let url = Url::parse(value).context("invalid evidence URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "evidence URL must be public HTTP(S) without credentials"
    );
    crate::transport::policy::check_url_secrets(&url)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl Evidence {
    pub(super) fn value(&self) -> Value {
        serde_json::json!({
            "artifact_path": self.artifact.path,
            "artifact_sha256": self.artifact.sha256,
            "artifact_bytes": self.artifact.bytes,
            "primary_url": self.primary_url,
            "captured_at": self.captured_at,
            "capture_kind": match self.capture_kind {
                CaptureKind::WebToolText => "web_tool_text",
                CaptureKind::BrowserDom => "browser_dom",
                CaptureKind::RetainedHtml => "retained_html",
            }
        })
    }
}

impl ResultLink {
    pub(super) fn evidence_values(&self, evidence: &[Evidence]) -> Result<Value> {
        self.result_evidence
            .iter()
            .chain(self.identity_evidence.iter())
            .map(|index| {
                evidence
                    .get(*index)
                    .map(Evidence::value)
                    .context("validated evidence reference disappeared")
            })
            .collect::<Result<Vec<_>>>()
            .map(Value::Array)
    }
}

impl Prepared {
    pub(super) fn evidence(&self) -> &[Evidence] {
        &self.manifest.evidence
    }

    pub(super) fn links(&self) -> &[ResultLink] {
        &self.manifest.changes
    }

    pub(super) fn reviewed_by(&self) -> &str {
        &self.manifest.reviewed_by
    }

    pub(super) fn researcher(&self) -> &str {
        &self.manifest.researcher
    }

    pub(super) fn reviewed_at(&self) -> &str {
        &self.manifest.reviewed_at
    }

    pub(super) fn authorization_quote(&self) -> &str {
        &self.manifest.authorization_quote
    }

    pub(super) fn sha256(&self) -> &str {
        &self.sha256
    }
}

impl ResultLink {
    pub(super) fn key(&self) -> &str {
        &self.source_result_key
    }
}
