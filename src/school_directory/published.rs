use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use serde::Deserialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

use super::{Directory, EnrichmentReport, Entry, digest, enrich_transaction, nonempty};

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
        let bytes = fs::read(path).context("reading published school address CSV")?;
        let sha256 = digest(&bytes);
        if !expected_sha256.eq_ignore_ascii_case(&sha256) {
            bail!("published school address CSV SHA-256 does not match the supplied digest");
        }
        let rows = csv::Reader::from_reader(bytes.as_slice())
            .deserialize::<Record>()
            .map(|row| row.context("parsing published school address row"))
            .collect::<Result<Vec<_>>>()?;
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

async fn insert_batch(
    connection: &mut PgConnection,
    rows: &[Record],
    directory: &PublishedDirectory,
) -> Result<u64> {
    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO schools(source,source_id,name,state,city,address,postal_code,source_url,address_provenance) ",
    );
    query.push_values(rows, |mut values, row| {
        let provenance = json!({"source": row.source, "nces_id": row.unit_id, "artifact_path": directory.path, "artifact_origin": directory.origin, "artifact_sha256": directory.sha256, "address_type": "unspecified_by_artifact", "hash_verification": "caller_supplied_sha256", "row_details": row_details(row)});
        let provenance = json!({"record": &provenance, "address": nonempty(&row.address).map(|_| &provenance), "postal_code": nonempty(&row.zip_code).map(|_| &provenance)});
        values.push_bind("nces").push_bind(&row.unit_id).push_bind(&row.name)
            .push_bind(nonempty(&row.state)).push_bind(nonempty(&row.city))
            .push_bind(nonempty(&row.address)).push_bind(nonempty(&row.zip_code))
            .push_bind(&directory.origin).push_bind(provenance);
    });
    query.push(" ON CONFLICT(source,source_id) DO NOTHING");
    Ok(query
        .build()
        .execute(connection)
        .await
        .context("inserting published schools")?
        .rows_affected())
}

pub async fn import(pool: &PgPool, directory: &PublishedDirectory) -> Result<ImportReport> {
    let mut transaction = pool.begin().await?;
    let enrichment = enrich_transaction(&mut transaction, &directory.directory).await?;
    let inserted = stream::iter(directory.rows.chunks(500).map(Ok::<_, anyhow::Error>))
        .try_fold(
            (&mut *transaction, 0_u64),
            |(connection, count), rows| async move {
                let inserted = insert_batch(connection, rows, directory).await?;
                Ok((connection, count.saturating_add(inserted)))
            },
        )
        .await?
        .1;
    transaction.commit().await?;
    Ok(ImportReport {
        inserted,
        enrichment,
    })
}
