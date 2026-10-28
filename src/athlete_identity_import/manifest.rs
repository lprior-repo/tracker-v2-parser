use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::Source;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profile {
    pub source: Source,
    pub source_id: String,
    pub expected_athlete_id: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub schema_revision: u32,
    pub canonical_athlete_id: i64,
    pub preferred_name: String,
    pub accepted_names: Vec<String>,
    pub reviewed_by: String,
    pub reason: String,
    pub profiles: [Profile; 2],
}

pub(super) struct Reviewed {
    pub manifest: Manifest,
    pub sha256: String,
}

pub(super) fn normalized(value: &str) -> String {
    value.trim().to_lowercase()
}

impl Manifest {
    pub fn accepts(&self, name: &str) -> bool {
        self.accepted_names
            .iter()
            .any(|accepted| normalized(accepted) == normalized(name))
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_revision == 1 && self.reviewed_by == "user",
            "identity manifest requires schema 1 and explicit user review"
        );
        ensure!(
            self.canonical_athlete_id > 0
                && self.profiles.iter().map(|p| p.expected_athlete_id).min()
                    == Some(self.canonical_athlete_id),
            "canonical identity must be the smallest expected existing athlete ID"
        );
        ensure!(
            !self.reason.trim().is_empty() && self.reason.len() <= 4096,
            "identity review requires a bounded human decision reason"
        );
        ensure!(
            (1..=16).contains(&self.accepted_names.len())
                && self
                    .accepted_names
                    .iter()
                    .all(|name| !name.trim().is_empty() && name.len() <= 256),
            "identity review requires 1..16 explicit bounded names"
        );
        ensure!(
            !self.preferred_name.trim().is_empty()
                && self.preferred_name.len() <= 256
                && self.accepts(&self.preferred_name),
            "preferred name must be explicitly accepted by the review"
        );
        ensure!(
            self.profiles.iter().all(|p| p.expected_athlete_id > 0
                && !p.source_id.trim().is_empty()
                && p.source_id.len() <= 128),
            "identity review requires two exact existing source profiles"
        );
        let [left, right] = &self.profiles;
        ensure!(
            left.source == right.source,
            "reviewed link requires two profiles from the same provider"
        );
        ensure!(
            (left.source, left.source_id.as_str()) != (right.source, right.source_id.as_str()),
            "identity review must identify distinct source profiles"
        );
        Ok(())
    }
}

pub(super) fn load(path: &Path, expected: &str) -> Result<Reviewed> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "identity manifest requires a SHA-256 digest"
    );
    let mut bytes = Vec::new();
    File::open(path)
        .context("opening reviewed identity manifest")?
        .take(16_385)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16_384, "identity manifest exceeds 16 KiB");
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    ensure!(
        sha256.eq_ignore_ascii_case(expected),
        "identity manifest SHA-256 mismatch"
    );
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("parsing reviewed identity manifest")?;
    manifest.validate()?;
    Ok(Reviewed { manifest, sha256 })
}
