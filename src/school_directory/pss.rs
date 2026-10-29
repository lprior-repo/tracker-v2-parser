use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use super::{
    Directory, EnrichmentReport, Entry, ImportCounts, MAX_IMPORT_BYTES, MAX_IMPORT_ROWS, digest,
    enrich_transaction, nonempty, read_limited, upsert_import,
};

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "PPIN")]
    id: String,
    #[serde(rename = "PINST")]
    name: String,
    #[serde(rename = "PADDRS")]
    address: String,
    #[serde(rename = "PCITY")]
    city: String,
    #[serde(rename = "PSTABB")]
    state: String,
    #[serde(rename = "PZIP")]
    zip: String,
    #[serde(rename = "PZIP4")]
    zip4: String,
    #[serde(rename = "PPHONE")]
    phone: String,
    #[serde(rename = "NUMSTUDS")]
    enrollment: String,
    #[serde(rename = "LATITUDE24", default)]
    latitude: String,
    #[serde(rename = "LONGITUDE24", default)]
    longitude: String,
    #[serde(rename = "LOGR2024", default)]
    low_grade: String,
    #[serde(rename = "HIGR2024", default)]
    high_grade: String,
}

pub struct PrivateDirectory {
    rows: Vec<Record>,
    directory: Directory,
    path: String,
    sha256: String,
}

pub struct ImportReport {
    pub inserted: u64,
    pub updated: u64,
    pub conflicts: u64,
    pub enrichment: EnrichmentReport,
}

fn postal(row: &Record) -> String {
    match (nonempty(&row.zip), nonempty(&row.zip4)) {
        (Some(zip), Some(plus4)) => format!("{zip}-{plus4}"),
        (Some(zip), None) => zip.to_owned(),
        _ => String::new(),
    }
}

fn details(row: &Record) -> Value {
    json!({"phone": nonempty(&row.phone), "enrollment": nonempty(&row.enrollment), "latitude": nonempty(&row.latitude), "longitude": nonempty(&row.longitude), "low_grade": nonempty(&row.low_grade), "high_grade": nonempty(&row.high_grade), "raw_zip": nonempty(&row.zip), "raw_zip4": nonempty(&row.zip4)})
}

impl PrivateDirectory {
    pub fn load(path: &Path, expected_sha256: &str) -> Result<Self> {
        let bytes = read_limited(path, MAX_IMPORT_BYTES).context("reading NCES PSS CSV")?;
        let sha256 = digest(&bytes);
        if !expected_sha256.eq_ignore_ascii_case(&sha256) {
            bail!("NCES PSS CSV SHA-256 does not match the supplied digest");
        }
        let mut rows = csv::Reader::from_reader(bytes.as_slice())
            .deserialize::<Record>()
            .take(MAX_IMPORT_ROWS.saturating_add(1))
            .map(|row| row.context("parsing NCES PSS row"))
            .collect::<Result<Vec<_>>>()?;
        if rows.len() > MAX_IMPORT_ROWS {
            bail!("NCES PSS CSV exceeds the {MAX_IMPORT_ROWS} row limit");
        }
        let mut ids = HashSet::with_capacity(rows.len());
        rows.iter_mut().try_for_each(|row| {
            row.id = row.id.trim().to_ascii_uppercase();
            if row.id.len() != 8
                || !row.id.bytes().all(|c| c.is_ascii_alphanumeric())
                || row.name.trim().is_empty()
            {
                bail!("NCES PSS CSV contains an invalid school ID or name");
            }
            if !ids.insert(row.id.clone()) {
                bail!("NCES PSS CSV contains duplicate school IDs");
            }
            Ok(())
        })?;
        let path = path.to_string_lossy().into_owned();
        let provenance = json!({"artifact_path": path, "artifact_sha256": sha256, "source": "nces-pss", "school_year": "2023-24", "school_kind": "private", "address_type": "mailing", "hash_verification": "caller_supplied_sha256"});
        let entries = rows
            .iter()
            .map(|row| Entry {
                key: format!("pss:{}", row.id),
                name: row.name.clone(),
                street1: row.address.clone(),
                street2: String::new(),
                city: row.city.clone(),
                state: row.state.clone(),
                zip: postal(row),
                website: String::new(),
                sources: "nces-pss".to_owned(),
                details: Some(details(row)),
            })
            .collect();
        Ok(Self {
            rows,
            directory: Directory::from_entries(entries, provenance),
            path,
            sha256,
        })
    }
}

async fn import_rows(
    connection: &mut PgConnection,
    directory: &PrivateDirectory,
) -> Result<ImportCounts> {
    stream::iter(directory.rows.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, ImportCounts::default()),
            |(connection, mut counts), row| async move {
                let provenance = json!({"source": "nces-pss", "pss_id": row.id, "artifact_path": directory.path, "artifact_sha256": directory.sha256, "school_year": "2023-24", "school_kind": "private", "address_type": "mailing", "hash_verification": "caller_supplied_sha256", "row_details": details(row)});
                let incoming = json!({"name": nonempty(&row.name), "state": nonempty(&row.state), "city": nonempty(&row.city), "address": nonempty(&row.address), "postal_code": nonempty(&postal(row)), "source_url": directory.path});
                let result = upsert_import(connection, &format!("pss:{}", row.id), incoming, provenance).await?;
                counts.inserted = counts.inserted.saturating_add(result.inserted);
                counts.updated = counts.updated.saturating_add(result.updated);
                counts.conflicts = counts.conflicts.saturating_add(result.conflicts);
                Ok((connection, counts))
            },
        )
        .await
        .map(|(_, counts)| counts)
}

pub async fn import(pool: &PgPool, directory: &PrivateDirectory) -> Result<ImportReport> {
    let mut transaction = pool.begin().await?;
    let report = import_transaction(&mut transaction, directory).await?;
    transaction.commit().await?;
    Ok(report)
}

pub(crate) async fn import_transaction(
    connection: &mut PgConnection,
    directory: &PrivateDirectory,
) -> Result<ImportReport> {
    let enrichment = enrich_transaction(connection, &directory.directory).await?;
    let counts = import_rows(connection, directory).await?;
    Ok(ImportReport {
        inserted: counts.inserted,
        updated: counts.updated,
        conflicts: counts.conflicts,
        enrichment,
    })
}
