use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub(super) const FORMAT: &str = "track-scrapper-migration-v2";
pub(super) const LEGACY: &str = "track-scrapper-migration-v1";
pub(super) const MAX_FILES: usize = 250_000;
pub(super) const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
pub(super) const SMALL: u64 = 1024 * 1024;
pub(super) const TABLES: [(&str, &str); 13] = [
    ("_sqlx_migrations", "version"),
    ("athletes", "id"),
    ("athlete_sources", "id"),
    ("schools", "id"),
    ("results", "id"),
    ("contacts", "id"),
    ("scrape_pages", "id"),
    ("durable_page_operations", "operation_id"),
    ("durable_page_attempts", "attempt_id"),
    ("durable_page_receipts", "operation_id"),
    ("durable_operation_receipts", "operation_key"),
    ("durable_wire_pacing", "pacing_key"),
    ("durable_wire_requests", "wire_id"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fingerprint {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Table {
    pub name: String,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Inventory {
    pub tables: Vec<Table>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub format: String,
    pub created_at_utc: String,
    pub source_postgres_major: u32,
    pub source_postgres_version: String,
    pub database_dump: String,
    pub config: String,
    pub evidence_dir: String,
    pub exports_dir: String,
    pub inventory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<crate::durable::page::ArtifactRef>,
    pub files: Vec<Fingerprint>,
}

pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Manifest {
    pub fn validate(&self, actual: &[Fingerprint], inventory: &Inventory) -> Result<()> {
        ensure!(
            matches!(self.format.as_str(), FORMAT | LEGACY),
            "unsupported migration format"
        );
        ensure!(
            self.source_postgres_major >= 13 && self.source_postgres_major <= 99,
            "PostgreSQL13+ required"
        );
        ensure!(
            self.source_postgres_version.len() <= 256
                && self
                    .source_postgres_version
                    .starts_with(&format!("{}.", self.source_postgres_major))
                && !self.source_postgres_version.chars().any(char::is_control),
            "invalid source version"
        );
        let timestamp = chrono::DateTime::parse_from_rfc3339(&self.created_at_utc)?;
        ensure!(
            timestamp.offset().local_minus_utc() == 0,
            "manifest timestamp must be UTC"
        );
        ensure!(
            self.database_dump == "database.dump"
                && self.config == "config.toml"
                && self.evidence_dir == "evidence"
                && self.exports_dir == "exports"
                && self.inventory == "inventory.json",
            "unsupported fixed archive paths"
        );
        ensure!(
            (self.format == FORMAT && self.artifacts_dir.as_deref() == Some("durable-artifacts"))
                || (self.format == LEGACY && self.artifacts_dir.is_none()),
            "artifact scope differs from archive version"
        );
        ensure!(
            self.files == actual && self.files.len() <= MAX_FILES,
            "archive file hashes, sizes or manifest list differ"
        );
        ensure!(
            self.format != LEGACY || self.required_artifacts.is_empty(),
            "legacy archives cannot claim verified durable artifact references"
        );
        ensure!(
            self.files.windows(2).all(|pair| pair
                .first()
                .zip(pair.get(1))
                .is_some_and(|(a, b)| a.path < b.path)),
            "manifest entries must be unique and sorted"
        );
        ensure!(
            ["database.dump", "config.toml", "inventory.json"]
                .iter()
                .all(|name| self.files.iter().any(|file| file.path == *name)),
            "required payload is missing"
        );
        let count = if self.format == LEGACY {
            7
        } else {
            TABLES.len()
        };
        ensure!(
            inventory.tables.len() == count
                && inventory
                    .tables
                    .iter()
                    .zip(TABLES)
                    .all(|(table, (name, _))| table.name == name && digest(&table.sha256)),
            "invalid table inventory"
        );
        Ok(())
    }
}
