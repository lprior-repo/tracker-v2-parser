use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{School, Source, Target};

#[tracing::instrument(skip(connection, schools))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    target: &Target,
    schools: &[School],
    url: &str,
) -> Result<()> {
    stream::iter(schools.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, school| async move {
            save(connection, target, school, url).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, school))]
async fn save(
    connection: &mut PgConnection,
    target: &Target,
    school: &School,
    url: &str,
) -> Result<()> {
    let source = target.source;
    let details = serde_json::to_value(school)?;
    let public_email = school.public_email.as_deref();
    let (id, provenance): (i64, Option<serde_json::Value>) = sqlx::query_as(
        "INSERT INTO schools(source,source_id,name,state,city,address,postal_code,website,public_email,source_url) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,NULLIF($9,''),$10) ON CONFLICT(source,source_id) DO UPDATE SET \
         name=CASE WHEN (schools.address_provenance->'manual_research'->-1->'identity_snapshot') ? 'name' AND schools.address_provenance->'manual_research'->-1->'identity_snapshot'->>'name' IS DISTINCT FROM EXCLUDED.name THEN schools.name ELSE EXCLUDED.name END, \
         state=CASE WHEN NULLIF(btrim(schools.address_provenance->'manual_research'->-1->'identity_snapshot'->>'state'),'') IS NOT NULL AND schools.address_provenance->'manual_research'->-1->'identity_snapshot'->'state' IS DISTINCT FROM to_jsonb(EXCLUDED.state) THEN schools.state ELSE COALESCE(EXCLUDED.state,schools.state) END, \
         city=CASE WHEN NULLIF(btrim(schools.address_provenance->'manual_research'->-1->'identity_snapshot'->>'city'),'') IS NOT NULL AND schools.address_provenance->'manual_research'->-1->'identity_snapshot'->'city' IS DISTINCT FROM to_jsonb(EXCLUDED.city) THEN schools.city ELSE COALESCE(EXCLUDED.city,schools.city) END, \
         address=CASE WHEN NULLIF(btrim(schools.address_provenance->'manual_research'->-1->'new'->>'address'),'') IS NOT NULL THEN schools.address ELSE COALESCE(EXCLUDED.address,schools.address) END, \
         postal_code=CASE WHEN NULLIF(btrim(schools.address_provenance->'manual_research'->-1->'new'->>'postal_code'),'') IS NOT NULL THEN schools.postal_code ELSE COALESCE(EXCLUDED.postal_code,schools.postal_code) END, \
         website=COALESCE(EXCLUDED.website,schools.website), \
         address_provenance=NULLIF(CASE WHEN EXCLUDED.postal_code IS NOT NULL THEN (CASE WHEN EXCLUDED.address IS NOT NULL THEN COALESCE(schools.address_provenance, '{}'::jsonb) - 'address' ELSE COALESCE(schools.address_provenance, '{}'::jsonb) END) - 'postal_code' WHEN EXCLUDED.address IS NOT NULL THEN COALESCE(schools.address_provenance, '{}'::jsonb) - 'address' ELSE COALESCE(schools.address_provenance, '{}'::jsonb) END, '{}'::jsonb), \
         public_email=COALESCE(EXCLUDED.public_email,schools.public_email),source_url=EXCLUDED.source_url,last_checked=clock_timestamp() RETURNING id,address_provenance",
    ).bind(source.as_str()).bind(&school.id).bind(&school.name).bind(&school.state)
        .bind(&school.city).bind(&school.address).bind(&school.postal_code).bind(&school.website)
        .bind(public_email).bind(url).fetch_one(&mut *connection).await?;
    let mut conflicts = research_conflicts(provenance.as_ref(), school, url, &details)?;
    let existing_conflicts = provenance
        .as_ref()
        .and_then(|value| value.get("manual_research_conflicts"));
    ensure!(
        existing_conflicts.is_none_or(serde_json::Value::is_array),
        "existing manual research conflict provenance is malformed"
    );
    if let Some(existing) = existing_conflicts.and_then(serde_json::Value::as_array) {
        conflicts.retain(|conflict| !existing.contains(conflict));
    }
    if !conflicts.is_empty() {
        sqlx::query("UPDATE schools SET address_provenance=jsonb_set(COALESCE(address_provenance,'{}'::jsonb),'{manual_research_conflicts}',COALESCE(address_provenance->'manual_research_conflicts','[]'::jsonb)||$2::jsonb,true) WHERE id=$1")
            .bind(id)
            .bind(serde_json::to_value(conflicts)?)
            .execute(&mut *connection)
            .await?;
    }
    save_office(connection, id, public_email, url, &target.url, &details).await
}

fn research_conflicts(
    provenance: Option<&serde_json::Value>,
    school: &School,
    source_url: &str,
    details: &serde_json::Value,
) -> Result<Vec<serde_json::Value>> {
    let Some(records) = provenance.and_then(|value| value.get("manual_research")) else {
        return Ok(Vec::new());
    };
    let records = records
        .as_array()
        .context("manual research address provenance is malformed")?;
    let Some(review) = records.last() else {
        return Ok(Vec::new());
    };
    let identity = review
        .get("identity_snapshot")
        .context("manual research identity snapshot is missing")?;
    let reviewed_fields = review
        .get("new")
        .context("manual research address snapshot is missing")?;
    let mut conflicts = Vec::new();
    add_conflict(
        &mut conflicts,
        "name",
        identity.get("name"),
        Some(serde_json::json!(school.name)),
        source_url,
        details,
    );
    add_identity_conflict(
        &mut conflicts,
        "city",
        identity.get("city"),
        school.city.as_deref(),
        source_url,
        details,
    );
    add_identity_conflict(
        &mut conflicts,
        "state",
        identity.get("state"),
        school.state.as_deref(),
        source_url,
        details,
    );
    add_researched_field_conflict(
        &mut conflicts,
        "address",
        reviewed_fields.get("address"),
        school.address.as_deref(),
        source_url,
        details,
    );
    add_researched_field_conflict(
        &mut conflicts,
        "postal_code",
        reviewed_fields.get("postal_code"),
        school.postal_code.as_deref(),
        source_url,
        details,
    );
    Ok(conflicts)
}

fn add_identity_conflict(
    conflicts: &mut Vec<serde_json::Value>,
    field: &str,
    reviewed: Option<&serde_json::Value>,
    incoming: Option<&str>,
    source_url: &str,
    details: &serde_json::Value,
) {
    let reviewed =
        reviewed.filter(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()));
    let incoming = incoming.filter(|value| !value.trim().is_empty());
    if let (Some(reviewed), Some(incoming)) = (reviewed, incoming) {
        add_conflict(
            conflicts,
            field,
            Some(reviewed),
            Some(serde_json::json!(incoming)),
            source_url,
            details,
        );
    }
}

fn add_researched_field_conflict(
    conflicts: &mut Vec<serde_json::Value>,
    field: &str,
    reviewed: Option<&serde_json::Value>,
    incoming: Option<&str>,
    source_url: &str,
    details: &serde_json::Value,
) {
    let reviewed =
        reviewed.filter(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()));
    let incoming = incoming.filter(|value| !value.trim().is_empty());
    if let (Some(reviewed), Some(incoming)) = (reviewed, incoming) {
        add_conflict(
            conflicts,
            field,
            Some(reviewed),
            Some(serde_json::json!(incoming)),
            source_url,
            details,
        );
    }
}

fn add_conflict(
    conflicts: &mut Vec<serde_json::Value>,
    field: &str,
    reviewed: Option<&serde_json::Value>,
    incoming: Option<serde_json::Value>,
    source_url: &str,
    details: &serde_json::Value,
) {
    if let (Some(reviewed), Some(incoming)) = (reviewed, incoming)
        && reviewed != &incoming
    {
        conflicts.push(serde_json::json!({
            "needs_review":true,
            "field":field,
            "reviewed_value":reviewed,
            "incoming_value":incoming,
            "incoming_source_url":source_url,
            "incoming_record":details
        }));
    }
}

#[tracing::instrument(skip(connection, email, details))]
async fn save_office(
    connection: &mut PgConnection,
    school: i64,
    email: Option<&str>,
    url: &str,
    checkpoint_url: &str,
    details: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO contacts(school_id,kind,role,email,source_url,raw,source_key) \
         SELECT $1,'school_office','',$2,$3,$4 || jsonb_build_object('checkpoint_url',$5::text), \
         encode(sha256(convert_to(jsonb_build_array($3::text,NULL::text,''::text,NULL::text,lower(btrim($2::text)))::text,'UTF8')),'hex') \
         WHERE $2::text IS NOT NULL AND btrim($2) <> '' \
         ON CONFLICT(school_id,kind,source_key) DO UPDATE SET email=EXCLUDED.email, \
         raw=EXCLUDED.raw,last_checked=clock_timestamp()",
    )
    .bind(school)
    .bind(email)
    .bind(url)
    .bind(details)
    .bind(checkpoint_url)
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection))]
pub(super) async fn find(
    connection: &mut PgConnection,
    source: Source,
    source_id: Option<&str>,
) -> Result<Option<i64>> {
    let Some(source_id) = source_id else {
        return Ok(None);
    };
    sqlx::query_scalar("SELECT id FROM schools WHERE source=$1 AND source_id=$2")
        .bind(source.as_str())
        .bind(source_id)
        .fetch_optional(connection)
        .await?
        .with_context(|| {
            format!(
                "unknown {} school ID {source_id}; import its observed school first",
                source.as_str()
            )
        })
        .map(Some)
}
