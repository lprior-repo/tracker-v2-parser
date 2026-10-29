use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use serde::Deserialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};

use super::{
    Directory, EnrichmentReport, Entry, ImportCounts, MAX_IMPORT_BYTES, MAX_IMPORT_ROWS, digest,
    enrich_transaction, nonempty, read_limited, upsert_import,
};

#[derive(Deserialize)]
struct Record {
    unit_id: String,
    name: String,
    address: String,
    city: String,
    state: String,
    zip_code: String,
    #[serde(default)]
    latitude: String,
    #[serde(default)]
    longitude: String,
    #[serde(default)]
    validated: String,
    #[serde(default)]
    geocode_status: String,
    source: String,
}

pub struct PublishedDirectory {
    rows: Vec<Record>,
    directory: Directory,
    path: String,
    origin: String,
    sha256: String,
}

pub struct ImportReport {
    pub inserted: u64,
    pub updated: u64,
    pub conflicts: u64,
    pub enrichment: EnrichmentReport,
}

impl PublishedDirectory {
    pub fn load(path: &Path, expected_sha256: &str) -> Result<Self> {
        Self::load_with_origin(path, expected_sha256, None)
    }

    pub fn load_with_origin(
        path: &Path,
        expected_sha256: &str,
        origin: Option<&str>,
    ) -> Result<Self> {
        let bytes =
            read_limited(path, MAX_IMPORT_BYTES).context("reading published school address CSV")?;
        let sha256 = digest(&bytes);
        if !expected_sha256.eq_ignore_ascii_case(&sha256) {
            bail!("published school address CSV SHA-256 does not match the supplied digest");
        }
        let rows = csv::Reader::from_reader(bytes.as_slice())
            .deserialize::<Record>()
            .take(MAX_IMPORT_ROWS.saturating_add(1))
            .map(|row| row.context("parsing published school address row"))
            .collect::<Result<Vec<_>>>()?;
        if rows.len() > MAX_IMPORT_ROWS {
            bail!("published school CSV exceeds the {MAX_IMPORT_ROWS} row limit");
        }
        let mut identities = HashSet::with_capacity(rows.len());
        rows.iter().try_for_each(|row| {
            if row.unit_id.len() != 12
                || !row.unit_id.bytes().all(|c| c.is_ascii_digit())
                || row.name.trim().is_empty()
                || row.source != "NCES_CSV"
            {
                bail!("published CSV contains an invalid or unsupported NCES row");
            }
            if !identities.insert(&row.unit_id) {
                bail!("published CSV contains duplicate NCES school IDs");
            }
            Ok(())
        })?;
        let path = path.to_string_lossy().into_owned();
        let origin = origin.map_or_else(|| path.clone(), str::to_owned);
        let provenance = json!({"artifact_path": path, "artifact_origin": origin, "artifact_sha256": sha256, "source": "NCES_CSV", "address_type": "unspecified_by_artifact", "hash_verification": "caller_supplied_sha256"});
        let entries = rows
            .iter()
            .map(|row| Entry {
                key: row.unit_id.clone(),
                name: row.name.clone(),
                street1: row.address.clone(),
                street2: String::new(),
                city: row.city.clone(),
                state: row.state.clone(),
                zip: row.zip_code.clone(),
                website: String::new(),
                sources: row.source.clone(),
                details: Some(row_details(row)),
            })
            .collect();
        Ok(Self {
            rows,
            directory: Directory::from_entries(entries, provenance),
            path,
            origin,
            sha256,
        })
    }
}
fn row_details(row: &Record) -> serde_json::Value {
    json!({"latitude": nonempty(&row.latitude), "longitude": nonempty(&row.longitude), "validated": nonempty(&row.validated), "geocode_status": nonempty(&row.geocode_status)})
}

async fn import_rows(
    connection: &mut PgConnection,
    directory: &PublishedDirectory,
) -> Result<ImportCounts> {
    stream::iter(directory.rows.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, ImportCounts::default()),
            |(connection, mut counts), row| async move {
                let provenance = json!({"source": row.source, "nces_id": row.unit_id, "artifact_path": directory.path, "artifact_origin": directory.origin, "artifact_sha256": directory.sha256, "address_type": "unspecified_by_artifact", "hash_verification": "caller_supplied_sha256", "row_details": row_details(row)});
                let incoming = json!({"name": nonempty(&row.name), "state": nonempty(&row.state), "city": nonempty(&row.city), "address": nonempty(&row.address), "postal_code": nonempty(&row.zip_code), "source_url": directory.origin});
                let result = upsert_import(connection, &row.unit_id, incoming, provenance).await?;
                counts.inserted = counts.inserted.saturating_add(result.inserted);
                counts.updated = counts.updated.saturating_add(result.updated);
                counts.conflicts = counts.conflicts.saturating_add(result.conflicts);
                Ok((connection, counts))
            },
        )
        .await
        .map(|(_, counts)| counts)
}

pub async fn import(pool: &PgPool, directory: &PublishedDirectory) -> Result<ImportReport> {
    let mut transaction = pool.begin().await?;
    let report = import_transaction(&mut transaction, directory).await?;
    transaction.commit().await?;
    Ok(report)
}

pub(crate) async fn import_transaction(
    connection: &mut PgConnection,
    directory: &PublishedDirectory,
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
