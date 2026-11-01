use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{lock_school, provenance, verify_school};
use crate::{
    database::Database,
    durable::operations::research::manifest::{Change, Manifest, SchoolWebsite},
    model::{School, Target},
};

#[tracing::instrument(skip_all)]
pub(super) async fn apply(
    connection: &mut sqlx::PgConnection,
    manifest: &Manifest,
    digest: &str,
    item: &SchoolWebsite,
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
    let (website, audit): (Option<String>, Option<Value>) =
        sqlx::query_as("SELECT website,website_provenance FROM schools WHERE id=$1")
            .bind(school.id)
            .fetch_one(&mut *connection)
            .await?;
    ensure!(
        website
            .as_deref()
            .is_none_or(|current| current.trim().is_empty() || current == item.website),
        "reviewed website conflicts with a populated database website"
    );
    ensure!(
        audit.as_ref().is_none_or(|value| {
            value.is_object()
                && value.get("manual_research").is_none_or(Value::is_array)
                && value
                    .get("manual_research_conflicts")
                    .is_none_or(Value::is_array)
        }),
        "existing manual website provenance is malformed"
    );
    let filled = website
        .as_deref()
        .is_none_or(|current| current.trim().is_empty());
    let record = record(manifest, digest, item, website.as_deref())?;
    sqlx::query("UPDATE schools SET website=$2,website_provenance=jsonb_set(COALESCE(website_provenance,'{}'::jsonb),'{manual_research}',COALESCE(website_provenance->'manual_research','[]'::jsonb)||jsonb_build_array($3::jsonb),true) WHERE id=$1 AND NOT COALESCE(website_provenance->'manual_research','[]'::jsonb) @> jsonb_build_array(jsonb_build_object('manifest_sha256',$4::text))")
        .bind(school.id).bind(&item.website).bind(record).bind(digest).execute(&mut *connection).await?;
    Ok(filled)
}

#[tracing::instrument(skip_all)]
pub(super) async fn enqueue(
    connection: &mut sqlx::PgConnection,
    changes: &[Change],
) -> Result<u64> {
    let mut targets: Vec<_> = changes
        .iter()
        .filter_map(|change| match change {
            Change::SchoolWebsite(item) => Some(contact_targets(item)),
            Change::SchoolAddress(_) | Change::Coach(_) => None,
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    targets.sort_by(|left, right| {
        (left.source, &left.url, &left.context.school_id).cmp(&(
            right.source,
            &right.url,
            &right.context.school_id,
        ))
    });
    if targets.is_empty() {
        return Ok(0);
    }
    Database::enqueue_transaction(connection, &targets).await
}

fn contact_targets(item: &SchoolWebsite) -> Result<Vec<Target>> {
    crate::sources::school_contacts(
        item.source,
        &School {
            id: item.source_id.clone(),
            name: item.expected_name.clone(),
            state: item.expected_state.clone(),
            website: Some(item.contact_url.clone()),
            ..School::default()
        },
        item.season,
    )
}

fn record(
    manifest: &Manifest,
    digest: &str,
    item: &SchoolWebsite,
    previous: Option<&str>,
) -> Result<Value> {
    Ok(json!({
        "reviewed_by":manifest.reviewed_by,
        "researcher":manifest.researcher,
        "reviewed_at":manifest.reviewed_at,
        "authorization_quote":manifest.authorization_quote,
        "manifest_sha256":digest,
        "source":item.source.as_str(),
        "source_id":item.source_id,
        "database_school_id":item.expected_school_id,
        "identity_snapshot":{"name":item.expected_name,"city":item.expected_city,"state":item.expected_state},
        "old_website":previous,
        "website":item.website,
        "contact_url":item.contact_url,
        "evidence":provenance::evidence_values(&manifest.evidence, &item.evidence)?
    }))
}
