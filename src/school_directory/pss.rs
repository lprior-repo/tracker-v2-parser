use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

use super::{Directory, EnrichmentReport, Entry, digest, enrich_transaction, nonempty};

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
        let bytes = fs::read(path).context("reading NCES PSS CSV")?;
        let sha256 = digest(&bytes);
        if !expected_sha256.eq_ignore_ascii_case(&sha256) {
            bail!("NCES PSS CSV SHA-256 does not match the supplied digest");
        }
        let mut rows = csv::Reader::from_reader(bytes.as_slice())
            .deserialize::<Record>()
            .map(|row| row.context("parsing NCES PSS row"))
            .collect::<Result<Vec<_>>>()?;
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

async fn insert_batch(
    connection: &mut PgConnection,
    rows: &[Record],
    directory: &PrivateDirectory,
) -> Result<u64> {
    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO schools(source,source_id,name,state,city,address,postal_code,source_url,address_provenance) ",
    );
    query.push_values(rows, |mut values, row| {
        let zip = postal(row);
        let provenance = json!({"source": "nces-pss", "pss_id": row.id, "artifact_path": directory.path, "artifact_sha256": directory.sha256, "school_year": "2023-24", "school_kind": "private", "address_type": "mailing", "hash_verification": "caller_supplied_sha256", "row_details": details(row)});
        let provenance = json!({"record": &provenance, "address": nonempty(&row.address).map(|_| &provenance), "postal_code": nonempty(&zip).map(|_| &provenance)});
        values.push_bind("nces").push_bind(format!("pss:{}", row.id)).push_bind(&row.name)
            .push_bind(nonempty(&row.state)).push_bind(nonempty(&row.city))
            .push_bind(nonempty(&row.address)).push_bind(nonempty(&zip).map(str::to_owned))
            .push_bind(&directory.path).push_bind(provenance);
    });
    query.push(" ON CONFLICT(source,source_id) DO NOTHING");
    Ok(query
        .build()
        .execute(connection)
        .await
        .context("inserting NCES PSS schools")?
        .rows_affected())
}

pub async fn import(pool: &PgPool, directory: &PrivateDirectory) -> Result<ImportReport> {
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
