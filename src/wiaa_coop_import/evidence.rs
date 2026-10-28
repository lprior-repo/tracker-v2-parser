use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::Source;

#[derive(Deserialize)]
pub(super) struct Manifest {
    pub schema_revision: u32,
    pub school_year: String,
    pub program_reports: Vec<Evidence>,
    pub associations: Vec<Association>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Evidence {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub request_form: BTreeMap<String, String>,
    pub status: u16,
    pub bytes: usize,
    pub sha256: String,
    pub captured_utc: String,
    pub artifact_path: PathBuf,
}

#[derive(Deserialize)]
pub(super) struct Association {
    pub provider_source: Source,
    pub provider_school_id: String,
    pub provider_school_name: String,
    pub provider_school_state: String,
    pub published_cooperative_name: String,
    pub designated_contact_school: String,
    pub host_wiaa_org_id: String,
    pub host_capture: Evidence,
    #[serde(default)]
    pub current_schedules: Vec<Evidence>,
}

pub(super) fn manifest(path: &Path) -> Result<(Manifest, String)> {
    let body = read(path, 256 * 1024)?;
    let value: Manifest = serde_json::from_slice(&body).context("decoding co-op manifest")?;
    ensure!(
        value.schema_revision == 1,
        "unsupported co-op manifest revision"
    );
    ensure!(
        value.school_year == "2026-2027",
        "co-op import requires 2026-2027 evidence"
    );
    ensure!(
        value.program_reports.len() == 4,
        "all four sport/gender reports are required"
    );
    ensure!(
        !value.associations.is_empty() && value.associations.len() <= 64,
        "co-op manifest requires 1..64 associations"
    );
    Ok((value, digest(&body)))
}

pub(super) fn capture(evidence: &Evidence, total: &mut usize) -> Result<String> {
    ensure!(evidence.status == 200, "co-op evidence must be HTTP 200");
    chrono::DateTime::parse_from_rfc3339(&evidence.captured_utc)
        .context("invalid evidence capture timestamp")?;
    let body = read(&evidence.artifact_path, 8 * 1024 * 1024)?;
    *total = total
        .checked_add(body.len())
        .context("evidence size overflow")?;
    ensure!(*total <= 64 * 1024 * 1024, "co-op evidence exceeds 64 MiB");
    ensure!(
        body.len() == evidence.bytes && digest(&body) == evidence.sha256,
        "co-op evidence byte count or SHA-256 mismatch: {}",
        evidence.artifact_path.display()
    );
    String::from_utf8(body).context("co-op evidence must be UTF-8 HTML")
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let capacity = u64::try_from(limit)?.saturating_add(1);
    let mut body = Vec::new();
    File::open(path)
        .with_context(|| format!("opening evidence {}", path.display()))?
        .take(capacity)
        .read_to_end(&mut body)?;
    ensure!(
        body.len() <= limit,
        "evidence exceeds byte capacity: {}",
        path.display()
    );
    Ok(body)
}

pub(super) fn digest(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}
