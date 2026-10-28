use std::{collections::HashMap, fs, io::Read, path::Path};

use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

pub mod pss;
pub mod published;

const MAX_IMPORT_BYTES: u64 = 134_217_728;
const MAX_MANIFEST_BYTES: u64 = 1_048_576;
const MAX_IMPORT_ROWS: usize = 250_000;

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path).context("opening school import file")?;
    if file.metadata()?.len() > limit {
        bail!("school import file exceeds the {limit} byte size limit");
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > limit {
        bail!("school import file exceeds the {limit} byte size limit");
    }
    Ok(bytes)
}
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
        let manifest_bytes = read_limited(manifest_path, MAX_MANIFEST_BYTES)
            .context("reading school directory manifest")?;
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
        let bytes =
            read_limited(csv_path, MAX_IMPORT_BYTES).context("reading school directory CSV")?;
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
            .take(MAX_IMPORT_ROWS.saturating_add(1))
            .map(|row| row.context("parsing school directory row"))
            .collect::<Result<Vec<_>>>()?;
        if rows.len() > MAX_IMPORT_ROWS {
            bail!("school directory exceeds the {MAX_IMPORT_ROWS} row limit");
        }
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
    address_provenance: Option<Value>,
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
    UpdatedConflict,
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
    if entry.key.trim().is_empty() || entry.sources.trim().is_empty() {
        return Ok(Outcome::Unchanged);
    }
    let owner = json!({"source": entry.sources, "source_id": entry.key});
    let provenance = school
        .address_provenance
        .as_ref()
        .filter(|value| value.is_object())
        .map_or_else(|| json!({}), Clone::clone);
    let related = ["address", "postal_code"]
        .iter()
        .any(|field| provenance.get(*field).and_then(|value| value.get("owner")) == Some(&owner));
    if !related
        && (!compatible(school.address.as_deref(), address.as_deref())
            || !compatible(school.postal_code.as_deref(), postal))
    {
        return Ok(Outcome::Conflict);
    }
    if !related && address.is_none() {
        return Ok(Outcome::Unchanged);
    }
    if !related && !corroborated(school, entry, address.as_deref(), directory, websites) {
        return Ok(Outcome::Uncorroborated);
    }
    let current = json!({"address": school.address, "postal_code": school.postal_code, "city": school.city, "state": school.state});
    let incoming = json!({"address": address, "postal_code": postal, "city": entry.city, "state": entry.state});
    let previous_details = provenance
        .pointer("/postal_code/row_details")
        .or_else(|| provenance.pointer("/address/row_details"));
    let details = merged_details(previous_details, entry.details.as_ref());
    let source = json!({"directory": directory.provenance, "directory_key": entry.key, "directory_sources": entry.sources, "row_details": details});
    let (values, provenance, changed, conflicted) = merge_fields(
        &current,
        &incoming,
        &provenance,
        &source,
        &owner,
        &["address", "postal_code"],
    );
    if !changed {
        return Ok(if conflicted {
            Outcome::Conflict
        } else {
            Outcome::Unchanged
        });
    }
    sqlx::query("UPDATE schools SET address=$2,postal_code=$3,address_provenance=$4 WHERE id=$1")
        .bind(school.id)
        .bind(values.get("address").and_then(Value::as_str))
        .bind(values.get("postal_code").and_then(Value::as_str))
        .bind(provenance)
        .execute(connection)
        .await
        .context("updating school address")?;
    Ok(if conflicted {
        Outcome::UpdatedConflict
    } else {
        Outcome::Updated
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
    let schools: Vec<School> = sqlx::query_as("SELECT id,name,state,city,address,postal_code,website,address_provenance FROM schools WHERE source <> 'nces' ORDER BY id")
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
    let identities: HashMap<_, _> = directory
        .entries
        .values()
        .filter_map(Option::as_ref)
        .map(|entry| (entry.key.as_str(), entry))
        .collect();
    let candidates = schools.iter().filter_map(|school| {
        let previous = school.address_provenance.as_ref().and_then(|provenance| {
            ["address", "postal_code"].iter().find_map(|field| {
                let owner = provenance.get(*field)?.get("owner")?;
                let entry = identities.get(owner.get("source_id")?.as_str()?)?;
                (owner.get("source")?.as_str()? == entry.sources).then_some(*entry)
            })
        });
        if let Some(entry) = previous {
            return Some((school, entry));
        }
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
                        Outcome::UpdatedConflict => {
                            report.updated = report.updated.saturating_add(1);
                            report.conflicts = report.conflicts.saturating_add(1);
                        }
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

#[derive(Default)]
struct ImportCounts {
    inserted: u64,
    updated: u64,
    conflicts: u64,
}

fn field_decision(
    current: Option<&str>,
    incoming: Option<&str>,
    provenance: Option<&Value>,
    owner: &Value,
) -> (bool, bool) {
    let Some(incoming) = incoming.and_then(nonempty) else {
        return (false, false);
    };
    let Some(current) = current.and_then(nonempty) else {
        return (true, false);
    };
    if current == incoming {
        return (false, false);
    }
    let replaceable = provenance.is_some_and(|provenance| {
        provenance.get("owner") == Some(owner)
            && provenance.get("value").and_then(Value::as_str) == Some(current)
    });
    (replaceable, !replaceable)
}

fn field_provenance(source: &Value, owner: &Value, value: &str) -> Value {
    let mut provenance = source
        .as_object()
        .map_or_else(serde_json::Map::new, Clone::clone);
    provenance.insert("owner".to_owned(), owner.clone());
    provenance.insert("value".to_owned(), json!(value));
    Value::Object(provenance)
}

fn merged_details(existing: Option<&Value>, incoming: Option<&Value>) -> Value {
    let mut merged = existing
        .and_then(Value::as_object)
        .map_or_else(serde_json::Map::new, Clone::clone);
    if let Some(incoming) = incoming.and_then(Value::as_object) {
        incoming
            .iter()
            .filter(|(_, value)| !value.is_null())
            .for_each(|(key, value)| {
                merged.insert(key.clone(), value.clone());
            });
    }
    Value::Object(merged)
}

fn postal_field(field: &str) -> bool {
    ["address", "city", "state", "postal_code"].contains(&field)
}

fn postal_conflict(current: &Value, incoming: &Value, provenance: &Value, owner: &Value) -> bool {
    ["address", "city", "state", "postal_code"]
        .iter()
        .any(|field| {
            let current = current.get(*field).and_then(Value::as_str);
            let incoming = incoming.get(*field).and_then(Value::as_str);
            !compatible(current, incoming)
                && field_decision(current, incoming, provenance.get(*field), owner).1
        })
}

fn merge_fields(
    current: &Value,
    incoming: &Value,
    provenance: &Value,
    source: &Value,
    owner: &Value,
    fields: &[&str],
) -> (Value, Value, bool, bool) {
    let provenance_before = provenance;
    let tuple_conflict = postal_conflict(current, incoming, provenance, owner);
    let values = current
        .as_object()
        .map_or_else(serde_json::Map::new, Clone::clone);
    let provenance = provenance
        .as_object()
        .map_or_else(serde_json::Map::new, Clone::clone);
    let (values, provenance, changed, conflicted) = fields.iter().fold(
        (values, provenance, false, false),
        |(mut values, mut provenance, changed, conflicted), field| {
            let incoming = incoming.get(*field).and_then(Value::as_str);
            let (replace, conflict) = field_decision(
                values.get(*field).and_then(Value::as_str),
                incoming,
                provenance.get(*field),
                owner,
            );
            let replace = replace && !(tuple_conflict && postal_field(field));
            if let Some(value) = incoming.filter(|_| replace) {
                values.insert((*field).to_owned(), json!(value));
                provenance.insert((*field).to_owned(), field_provenance(source, owner, value));
            }
            (
                values,
                provenance,
                changed || replace,
                conflicted || conflict,
            )
        },
    );
    let mut updated_provenance = Value::Object(provenance);
    if let Some(evidence) = updated_provenance
        .as_object_mut()
        .filter(|_| tuple_conflict)
    {
        evidence.insert(
            "postal_conflict".to_owned(),
            json!({"incoming": incoming, "source": source, "owner": owner}),
        );
    }
    let evidence_changed = updated_provenance != *provenance_before;
    (
        Value::Object(values),
        updated_provenance,
        changed || evidence_changed,
        conflicted || tuple_conflict,
    )
}

async fn upsert_import(
    connection: &mut PgConnection,
    source_id: &str,
    incoming: Value,
    mut source: Value,
) -> Result<ImportCounts> {
    let existing: Option<(i64, Value, Option<Value>)> = sqlx::query_as(
        "SELECT id,jsonb_build_object('name',name,'state',state,'city',city,'address',address,'postal_code',postal_code,'source_url',source_url),address_provenance FROM schools WHERE source='nces' AND source_id=$1 FOR UPDATE",
    ).bind(source_id).fetch_optional(&mut *connection).await?;
    let owner = json!({"source": source.get("source"), "source_id": source_id});
    source
        .as_object_mut()
        .context("school import provenance must be an object")?
        .insert("owner".to_owned(), owner.clone());
    let Some((id, current, provenance)) = existing else {
        let mut provenance = [
            "name",
            "state",
            "city",
            "address",
            "postal_code",
            "source_url",
        ]
        .iter()
        .filter_map(|field| {
            incoming.get(*field).and_then(Value::as_str).map(|value| {
                (
                    (*field).to_owned(),
                    field_provenance(&source, &owner, value),
                )
            })
        })
        .collect::<serde_json::Map<_, _>>();
        provenance.insert("record".to_owned(), source);
        sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,postal_code,source_url,address_provenance) VALUES ('nces',$1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(source_id).bind(incoming.get("name").and_then(Value::as_str))
            .bind(incoming.get("state").and_then(Value::as_str))
            .bind(incoming.get("city").and_then(Value::as_str))
            .bind(incoming.get("address").and_then(Value::as_str))
            .bind(incoming.get("postal_code").and_then(Value::as_str))
            .bind(incoming.get("source_url").and_then(Value::as_str)).bind(Value::Object(provenance))
            .execute(connection).await.context("inserting imported school")?;
        return Ok(ImportCounts {
            inserted: 1,
            ..ImportCounts::default()
        });
    };
    update_import(connection, id, current, provenance, incoming, source, owner).await
}

async fn update_import(
    connection: &mut PgConnection,
    id: i64,
    current: Value,
    provenance: Option<Value>,
    incoming: Value,
    mut source: Value,
    owner: Value,
) -> Result<ImportCounts> {
    let provenance = match provenance {
        Some(value) if value.is_object() => value,
        _ => json!({}),
    };
    let record_owned = provenance.pointer("/record/owner") == Some(&owner);
    let previous_details = record_owned
        .then(|| provenance.pointer("/record/row_details"))
        .flatten();
    let details = merged_details(previous_details, source.get("row_details"));
    let details_changed = record_owned
        && previous_details != Some(&details)
        && details.as_object().is_some_and(|value| !value.is_empty());
    source
        .as_object_mut()
        .context("school import provenance must be an object")?
        .insert("row_details".to_owned(), details);
    let (values, mut updated_provenance, changed, conflicted) = merge_fields(
        &current,
        &incoming,
        &provenance,
        &source,
        &owner,
        &["name", "state", "city", "address", "postal_code"],
    );
    if !changed && !details_changed {
        return Ok(ImportCounts {
            conflicts: u64::from(conflicted),
            ..ImportCounts::default()
        });
    }
    let (values, url_provenance, _, url_conflict) = merge_fields(
        &values,
        &incoming,
        &updated_provenance,
        &source,
        &owner,
        &["source_url"],
    );
    updated_provenance = url_provenance;
    if record_owned || provenance.get("record").is_none() {
        updated_provenance
            .as_object_mut()
            .context("school field provenance must be an object")?
            .insert("record".to_owned(), source);
    }
    sqlx::query("UPDATE schools SET name=$2,state=$3,city=$4,address=$5,postal_code=$6,source_url=$7,address_provenance=$8 WHERE id=$1")
        .bind(id).bind(values.get("name").and_then(Value::as_str))
        .bind(values.get("state").and_then(Value::as_str))
        .bind(values.get("city").and_then(Value::as_str))
        .bind(values.get("address").and_then(Value::as_str))
        .bind(values.get("postal_code").and_then(Value::as_str))
        .bind(values.get("source_url").and_then(Value::as_str)).bind(updated_provenance)
        .execute(connection).await.context("correcting imported school")?;
    Ok(ImportCounts {
        updated: 1,
        conflicts: u64::from(conflicted || url_conflict),
        ..ImportCounts::default()
    })
}
