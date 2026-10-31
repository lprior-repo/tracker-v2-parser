mod provenance;

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde::Serialize;
use serde_json::{Value, json};

use crate::model::Source;

use super::manifest::{Change, Coach, Manifest, Reviewed, SchoolAddress};

#[derive(Debug, Serialize)]
pub struct Report {
    addresses_filled: u64,
    coaches_inserted: u64,
    coaches_already_present: u64,
    manifest_sha256: String,
}

#[derive(Clone)]
pub(super) struct SchoolRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) city: Option<String>,
    pub(super) state: Option<String>,
    pub(super) address: Option<String>,
    pub(super) postal_code: Option<String>,
    pub(super) address_provenance: Option<Value>,
}

pub(super) async fn apply(
    connection: &mut sqlx::PgConnection,
    reviewed: Reviewed,
) -> Result<Report> {
    let manifest = reviewed.manifest;
    let mut changes: Vec<_> = manifest.changes.iter().collect();
    changes.sort_by_key(|change| target_order(change));
    let (_, counts, _, _) = stream::iter(changes)
        .map(Ok::<_, anyhow::Error>)
        .try_fold(
            (
                connection,
                (0_u64, 0_u64, 0_u64),
                &manifest,
                reviewed.sha256.as_str(),
            ),
            |(connection, counts, manifest, sha256), change| async move {
                let counts = apply_change(connection, manifest, sha256, change, counts).await?;
                Ok((connection, counts, manifest, sha256))
            },
        )
        .await?;
    Ok(Report {
        addresses_filled: counts.0,
        coaches_inserted: counts.1,
        coaches_already_present: counts.2,
        manifest_sha256: reviewed.sha256,
    })
}

const fn target_order(change: &Change) -> (i64, &'static str) {
    match change {
        Change::SchoolAddress(item) => (item.expected_school_id, "address"),
        Change::Coach(item) => (item.expected_school_id, "coach"),
    }
}

fn apply_change<'a>(
    connection: &'a mut sqlx::PgConnection,
    manifest: &'a Manifest,
    manifest_sha256: &'a str,
    change: &'a Change,
    counts: (u64, u64, u64),
) -> futures::future::BoxFuture<'a, Result<(u64, u64, u64)>> {
    Box::pin(async move {
        match change {
            Change::SchoolAddress(item) => {
                apply_address(connection, manifest, manifest_sha256, item).await?;
                Ok((
                    counts
                        .0
                        .checked_add(1)
                        .context("reviewed address count overflow")?,
                    counts.1,
                    counts.2,
                ))
            }
            Change::Coach(item) => {
                let inserted = apply_coach(connection, manifest, manifest_sha256, item).await?;
                Ok((
                    counts.0,
                    counts
                        .1
                        .checked_add(u64::from(inserted))
                        .context("reviewed coach insert count overflow")?,
                    counts
                        .2
                        .checked_add(u64::from(!inserted))
                        .context("reviewed existing coach count overflow")?,
                ))
            }
        }
    })
}

async fn lock_school(
    connection: &mut sqlx::PgConnection,
    source: Source,
    source_id: &str,
) -> Result<SchoolRow> {
    sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<Value>)>(
        "SELECT id,name,city,state,address,postal_code,address_provenance FROM schools WHERE source=$1 AND source_id=$2 FOR UPDATE",
    )
    .bind(source.as_str())
    .bind(source_id)
    .fetch_optional(connection)
    .await?
    .map(|row| SchoolRow {
        id: row.0,
        name: row.1,
        city: row.2,
        state: row.3,
        address: row.4,
        postal_code: row.5,
        address_provenance: row.6,
    })
    .context("reviewed school source and source ID did not match a database school")
}

fn verify_school(
    row: &SchoolRow,
    source_id: &str,
    expected_id: i64,
    name: &str,
    city: Option<&String>,
    state: Option<&String>,
) -> Result<()> {
    ensure!(
        row.id == expected_id
            && row.name == *name
            && row.city.as_ref() == city
            && row.state.as_ref() == state,
        "reviewed school database ID, name, city, or state is stale or conflicting for source ID {source_id}"
    );
    Ok(())
}

async fn apply_address(
    connection: &mut sqlx::PgConnection,
    manifest: &Manifest,
    manifest_sha256: &str,
    item: &SchoolAddress,
) -> Result<()> {
    let row = lock_school(connection, item.source, &item.source_id).await?;
    verify_school(
        &row,
        &item.source_id,
        item.expected_school_id,
        &item.expected_name,
        item.expected_city.as_ref(),
        item.expected_state.as_ref(),
    )?;
    ensure!(
        row.address_provenance
            .as_ref()
            .and_then(|value| value.get("manual_research"))
            .is_none_or(Value::is_array),
        "existing manual research address provenance is malformed"
    );
    ensure!(
        is_blank(row.address.as_ref()) || row.address.as_deref() == Some(item.address.as_str()),
        "reviewed address conflicts with a populated database address"
    );
    ensure!(
        item.postal_code
            .as_deref()
            .is_none_or(|incoming| is_blank(row.postal_code.as_ref())
                || row.postal_code.as_deref() == Some(incoming)),
        "reviewed postal code conflicts with a populated database postal code"
    );
    ensure!(
        is_blank(row.address.as_ref())
            || item.postal_code.as_ref().is_some_and(
                |incoming| is_blank(row.postal_code.as_ref()) && !incoming.trim().is_empty()
            ),
        "reviewed address change has no blank address or postal field to fill"
    );
    let next_address = row
        .address
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some(item.address.clone()));
    let next_postal = row
        .postal_code
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| item.postal_code.clone());
    let provenance = provenance::address_record(
        manifest,
        manifest_sha256,
        item,
        &row,
        next_address.as_ref(),
        next_postal.as_ref(),
    )?;
    sqlx::query("UPDATE schools SET address=$2,postal_code=$3,address_provenance=jsonb_set(COALESCE(address_provenance,'{}'::jsonb),'{manual_research}',COALESCE(address_provenance->'manual_research','[]'::jsonb)||jsonb_build_array($4::jsonb),true) WHERE id=$1")
        .bind(row.id)
        .bind(next_address)
        .bind(next_postal)
        .bind(provenance)
        .execute(connection)
        .await?;
    Ok(())
}

fn is_blank(value: Option<&String>) -> bool {
    value
        .map(String::as_str)
        .is_none_or(|current| current.trim().is_empty())
}

async fn apply_coach(
    connection: &mut sqlx::PgConnection,
    manifest: &Manifest,
    manifest_sha256: &str,
    item: &Coach,
) -> Result<bool> {
    let school = lock_school(connection, item.source, &item.source_id).await?;
    verify_school(
        &school,
        &item.source_id,
        item.expected_school_id,
        &item.expected_name,
        item.expected_city.as_ref(),
        item.expected_state.as_ref(),
    )?;
    let coach_audit = provenance::coach_record(manifest, manifest_sha256, item, school.id)?;
    let provenance = json!({"manual_research":[coach_audit]});
    let primary_url = provenance::evidence_first_url(&manifest.evidence, &item.name_role_evidence)?;
    let existing: Option<(i64, Option<String>, Option<String>, Value)> = sqlx::query_as("SELECT id,name,sport,raw FROM contacts WHERE school_id=$1 AND kind='coach' AND source_key=encode(sha256(convert_to(jsonb_build_array($2::text,$3::text,$4::text,$5::text,lower(btrim($6::text)))::text,'UTF8')),'hex') FOR UPDATE")
        .bind(school.id)
        .bind(&primary_url)
        .bind(&item.name)
        .bind(&item.role)
        .bind(item.sport.as_str())
        .bind(&item.email)
        .fetch_optional(&mut *connection)
        .await?;
    if let Some((contact_id, name, sport, raw)) = existing {
        ensure!(
            name.as_deref() == Some(item.name.as_str())
                && sport.as_deref() == Some(item.sport.as_str()),
            "reviewed coach claim conflicts with an existing contact natural key"
        );
        let existing_audits = match raw.get("manual_research") {
            Some(Value::Array(audits)) => audits.clone(),
            Some(_) => anyhow::bail!("existing manual coach provenance is malformed"),
            None => Vec::new(),
        };
        ensure!(
            existing_audits.iter().all(|audit| audit
                .get("gender")
                .and_then(Value::as_str)
                .is_none_or(|value| value == provenance::gender_str(item.gender))),
            "reviewed coach gender conflicts with an existing manual review"
        );
        sqlx::query("UPDATE contacts SET raw=jsonb_set(raw,'{manual_research}',COALESCE(raw->'manual_research','[]'::jsonb)||$2::jsonb,true) WHERE id=$1")
            .bind(contact_id)
            .bind(json!([coach_audit]))
            .execute(connection)
            .await?;
        return Ok(false);
    }
    sqlx::query("INSERT INTO contacts(school_id,kind,name,role,email,sport,source_url,raw,source_key) VALUES($1,'coach',$2,$3,$4,$5,$6,$7,encode(sha256(convert_to(jsonb_build_array($6::text,$2::text,$3::text,$5::text,lower(btrim($4::text)))::text,'UTF8')),'hex'))")
        .bind(school.id)
        .bind(&item.name)
        .bind(&item.role)
        .bind(&item.email)
        .bind(item.sport.as_str())
        .bind(primary_url)
        .bind(provenance)
        .execute(connection)
        .await?;
    Ok(true)
}
