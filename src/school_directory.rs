use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

pub mod pss;
pub mod published;

#[derive(Deserialize)]
struct Manifest {
    schema_revision: u32,
    generation_digest: String,
    run: Value,
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    name: String,
    sha256: String,
    bytes: u64,
}

#[derive(Deserialize)]
struct Entry {
    key: String,
    name: String,
    street1: String,
    street2: String,
    city: String,
    state: String,
    zip: String,
    website: String,
    #[serde(skip)]
    details: Option<Value>,
    sources: String,
}

type Key = (String, String, String);

pub struct Directory {
    entries: HashMap<Key, Option<Entry>>,
    websites: HashMap<String, usize>,
    provenance: Value,
}

fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn key(name: &str, state: &str, city: &str) -> Option<Key> {
    let result = (normalized(name), normalized(state), normalized(city));
    if result.0.is_empty() || result.1.is_empty() || result.2.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Directory {
    pub fn load(csv_path: &Path, manifest_path: &Path) -> Result<Self> {
        let manifest_bytes =
            fs::read(manifest_path).context("reading school directory manifest")?;
        let manifest: Manifest =
            serde_json::from_slice(&manifest_bytes).context("parsing school directory manifest")?;
        if manifest.schema_revision != 1 || manifest.generation_digest.trim().is_empty() {
            bail!("unsupported or invalid school directory manifest");
        }
        let artifacts: Vec<_> = manifest
            .artifacts
            .iter()
            .filter(|a| a.name == "school_directory.csv")
            .collect();
        let [artifact] = artifacts.as_slice() else {
            bail!("manifest must contain exactly one school_directory.csv artifact");
        };
        let bytes = fs::read(csv_path).context("reading school directory CSV")?;
        let sha256 = digest(&bytes);
        if artifact.sha256 != sha256 || u64::try_from(bytes.len())? != artifact.bytes {
            bail!("school directory CSV does not match manifest hash and size");
        }
        let provenance = json!({
            "artifact": "school_directory.csv",
            "artifact_sha256": sha256,
            "manifest_sha256": digest(&manifest_bytes),
            "generation_digest": manifest.generation_digest,
            "run_id": manifest.run.get("run_id"),
            "input_lanes": manifest.run.pointer("/inputs/lanes").and_then(Value::as_array).map(|lanes| lanes.iter().map(|lane| json!({"source": lane.get("source"), "sha256": lane.get("sha256")})).collect::<Vec<_>>()),
            "address_type": "unspecified_by_directory"
        });
        let rows = csv::Reader::from_reader(bytes.as_slice())
            .deserialize::<Entry>()
            .map(|row| row.context("parsing school directory row"))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self::from_entries(rows, provenance))
    }

    fn from_entries(rows: Vec<Entry>, provenance: Value) -> Self {
        let (entries, websites) = rows.into_iter().fold(
            (HashMap::new(), HashMap::<String, usize>::new()),
            |(mut entries, mut websites), row| {
                if let Some(site) = website(&row.website) {
                    let count = websites.entry(site).or_insert(0);
                    *count = count.saturating_add(1);
                }
                if let Some(key) = key(&row.name, &row.state, &row.city) {
                    use std::collections::hash_map::Entry as MapEntry;
                    match entries.entry(key) {
                        MapEntry::Vacant(slot) => {
                            slot.insert(Some(row));
                        }
                        MapEntry::Occupied(mut slot) => {
                            slot.insert(None);
                        }
                    }
                }
                (entries, websites)
            },
        );
        Self {
            entries,
            websites,
            provenance,
        }
    }
}

#[derive(sqlx::FromRow)]
struct School {
    id: i64,
    name: String,
    state: Option<String>,
    city: Option<String>,
    address: Option<String>,
    website: Option<String>,
    postal_code: Option<String>,
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn website(value: &str) -> Option<String> {
    let value = nonempty(value)?;
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(parsed.into())
}

fn compatible(existing: Option<&str>, incoming: Option<&str>) -> bool {
    match (existing.and_then(nonempty), incoming.and_then(nonempty)) {
        (Some(left), Some(right)) => normalized(left) == normalized(right),
        _ => true,
    }
}

#[derive(Default)]
pub struct EnrichmentReport {
    pub updated: u64,
    pub conflicts: u64,
    pub uncorroborated: u64,
}

enum Outcome {
    Updated,
    Conflict,
    Uncorroborated,
    Unchanged,
}

fn corroborated(
    school: &School,
    entry: &Entry,
    address: Option<&str>,
    directory: &Directory,
    websites: &HashMap<String, usize>,
) -> bool {
    let street_matches = match (school.address.as_deref().and_then(nonempty), address) {
        (Some(left), Some(right)) => normalized(left) == normalized(right),
        _ => false,
    };
    let website_matches = school
        .website
        .as_deref()
        .and_then(website)
        .zip(website(&entry.website))
        .is_some_and(|(left, right)| {
            left == right
                && websites.get(&left) == Some(&1)
                && directory.websites.get(&left) == Some(&1)
        });
    street_matches || website_matches
}

async fn apply(
    connection: &mut PgConnection,
    school: &School,
    entry: &Entry,
    directory: &Directory,
    websites: &HashMap<String, usize>,
) -> Result<Outcome> {
    let address = match (nonempty(&entry.street1), nonempty(&entry.street2)) {
        (Some(first), Some(second)) => Some(format!("{first}, {second}")),
        (Some(first), None) => Some(first.to_owned()),
        _ => None,
    };
    let postal = nonempty(&entry.zip);
    if !compatible(school.address.as_deref(), address.as_deref())
        || !compatible(school.postal_code.as_deref(), postal)
    {
        return Ok(Outcome::Conflict);
    }
    if entry.key.trim().is_empty() || entry.sources.trim().is_empty() || address.is_none() {
        return Ok(Outcome::Unchanged);
    }
    if !corroborated(school, entry, address.as_deref(), directory, websites) {
        return Ok(Outcome::Uncorroborated);
    }
    let fill_address = school.address.as_deref().and_then(nonempty).is_none();
    let fill_postal =
        school.postal_code.as_deref().and_then(nonempty).is_none() && postal.is_some();
    if !fill_address && !fill_postal {
        return Ok(Outcome::Unchanged);
    }
    let source = json!({"directory": directory.provenance, "directory_key": entry.key, "directory_sources": entry.sources, "row_details": entry.details});
    let provenance = json!({"address": fill_address.then_some(&source), "postal_code": fill_postal.then_some(&source)});
    let result = sqlx::query("UPDATE schools SET address=CASE WHEN $2 THEN $3 ELSE address END, postal_code=CASE WHEN $4 THEN $5 ELSE postal_code END, address_provenance=COALESCE(address_provenance, '{}'::jsonb) || jsonb_strip_nulls($6::jsonb) WHERE id=$1")
        .bind(school.id).bind(fill_address).bind(address).bind(fill_postal).bind(postal).bind(provenance)
        .execute(connection).await.context("updating school address")?;
    Ok(if result.rows_affected() == 1 {
        Outcome::Updated
    } else {
        Outcome::Unchanged
    })
}

pub async fn enrich(pool: &PgPool, directory: &Directory) -> Result<EnrichmentReport> {
    let mut transaction = pool.begin().await?;
    let result = enrich_transaction(&mut transaction, directory).await?;
    transaction.commit().await?;
    Ok(result)
}

async fn enrich_transaction(
    connection: &mut PgConnection,
    directory: &Directory,
) -> Result<EnrichmentReport> {
    sqlx::query("LOCK TABLE schools IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *connection)
        .await?;
    let schools: Vec<School> = sqlx::query_as("SELECT id,name,state,city,address,postal_code,website FROM schools WHERE source <> 'nces' ORDER BY id")
        .fetch_all(&mut *connection).await?;
    let counts = schools
        .iter()
        .filter_map(|school| {
            key(
                &school.name,
                school.state.as_deref()?,
                school.city.as_deref()?,
            )
        })
        .fold(HashMap::<Key, usize>::new(), |mut counts, key| {
            let count = counts.entry(key).or_insert(0);
            *count = count.saturating_add(1);
            counts
        });
    let websites = schools
        .iter()
        .filter_map(|school| school.website.as_deref().and_then(website))
        .fold(HashMap::<String, usize>::new(), |mut counts, site| {
            let count = counts.entry(site).or_insert(0);
            *count = count.saturating_add(1);
            counts
        });
    let candidates = schools.iter().filter_map(|school| {
        let key = key(
            &school.name,
            school.state.as_deref()?,
            school.city.as_deref()?,
        )?;
        if counts.get(&key) != Some(&1) {
            return None;
        }
        match directory.entries.get(&key) {
            Some(Some(entry)) => Some((school, entry)),
            _ => None,
        }
    });
    let report = stream::iter(candidates.map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, EnrichmentReport::default()),
            |(connection, mut report), (school, entry)| {
                let websites = &websites;
                async move {
                    match apply(connection, school, entry, directory, websites).await? {
                        Outcome::Updated => report.updated = report.updated.saturating_add(1),
                        Outcome::Conflict => report.conflicts = report.conflicts.saturating_add(1),
                        Outcome::Uncorroborated => {
                            report.uncorroborated = report.uncorroborated.saturating_add(1);
                        }
                        Outcome::Unchanged => {}
                    }
                    Ok((connection, report))
                }
            },
        )
        .await?
        .1;
    Ok(report)
}
