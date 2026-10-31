use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::model::Gender;

use super::super::manifest::{Coach, Evidence, Manifest, SchoolAddress};
use super::SchoolRow;

pub(super) fn address_record(
    manifest: &Manifest,
    manifest_sha256: &str,
    item: &SchoolAddress,
    row: &SchoolRow,
    next_address: Option<&String>,
    next_postal: Option<&String>,
) -> Result<Value> {
    Ok(json!({
        "reviewed_by":manifest.reviewed_by,
        "researcher":manifest.researcher,
        "reviewed_at":manifest.reviewed_at,
        "authorization_quote":manifest.authorization_quote,
        "manifest_sha256":manifest_sha256,
        "source":item.source.as_str(),
        "source_id":item.source_id,
        "database_school_id":row.id,
        "identity_snapshot":{"name":row.name,"city":row.city,"state":row.state},
        "old":{"address":row.address,"postal_code":row.postal_code},
        "new":{"address":next_address,"postal_code":next_postal},
        "evidence":evidence_values(&manifest.evidence, &item.evidence)?
    }))
}

pub(super) fn coach_record(
    manifest: &Manifest,
    manifest_sha256: &str,
    item: &Coach,
    school_id: i64,
) -> Result<Value> {
    Ok(json!({
        "reviewed_by":manifest.reviewed_by,
        "researcher":manifest.researcher,
        "reviewed_at":manifest.reviewed_at,
        "authorization_quote":manifest.authorization_quote,
        "manifest_sha256":manifest_sha256,
        "source":item.source.as_str(),
        "source_id":item.source_id,
        "database_school_id":school_id,
        "researched_fields":{"name":item.name,"role":item.role,"email":item.email,"sport":item.sport.as_str(),"gender":gender_str(item.gender)},
        "gender":gender_str(item.gender),
        "sport":item.sport.as_str(),
        "role":item.role,
        "name_role_evidence":evidence_values(&manifest.evidence, &item.name_role_evidence)?,
        "email_evidence":evidence_values(&manifest.evidence, &item.email_evidence)?
    }))
}

pub(super) fn evidence_first_url(evidence: &[Evidence], references: &[usize]) -> Result<String> {
    let index = references
        .first()
        .context("coach name/role evidence is required")?;
    Ok(evidence
        .get(*index)
        .context("validated evidence reference disappeared")?
        .primary_url
        .clone())
}

fn evidence_values(evidence: &[Evidence], references: &[usize]) -> Result<Vec<Value>> {
    references
        .iter()
        .map(|index| {
            let item = evidence
                .get(*index)
                .context("validated evidence reference disappeared")?;
            Ok(json!({
                "artifact_path":item.artifact.path,
                "artifact_sha256":item.artifact.sha256,
                "artifact_bytes":item.artifact.bytes,
                "primary_url":item.primary_url,
                "captured_at":item.captured_at,
                "capture_kind":capture_kind(item.capture_kind)
            }))
        })
        .collect()
}

pub(super) const fn gender_str(gender: Gender) -> &'static str {
    match gender {
        Gender::Boys => "boys",
        Gender::Girls => "girls",
        Gender::Mixed => "mixed",
        Gender::Unknown => "unknown",
    }
}

const fn capture_kind(kind: super::super::manifest::CaptureKind) -> &'static str {
    match kind {
        super::super::manifest::CaptureKind::WebToolText => "web_tool_text",
        super::super::manifest::CaptureKind::BrowserDom => "browser_dom",
        super::super::manifest::CaptureKind::RetainedHtml => "retained_html",
    }
}
