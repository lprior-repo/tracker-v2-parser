use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sqlx::PgConnection;

use crate::model::School;

#[tracing::instrument(skip_all)]
pub(super) async fn retain_conflicts(
    connection: &mut PgConnection,
    school_id: i64,
    provenance: Option<&Value>,
    incoming: &School,
    source_url: &str,
    details: &Value,
) -> Result<()> {
    let Some(provenance) = provenance else {
        return Ok(());
    };
    let records = provenance
        .get("manual_research")
        .and_then(Value::as_array)
        .context("manual website research provenance is malformed")?;
    let Some(review) = records.last() else {
        return Ok(());
    };
    let mut conflicts = conflicts(review, incoming, source_url, details)?;
    let existing = provenance.get("manual_research_conflicts");
    ensure!(
        existing.is_none_or(Value::is_array),
        "manual website conflict provenance is malformed"
    );
    if let Some(existing) = existing.and_then(Value::as_array) {
        conflicts.retain(|conflict| !existing.contains(conflict));
    }
    if conflicts.is_empty() {
        return Ok(());
    }
    sqlx::query("UPDATE schools SET website_provenance=jsonb_set(website_provenance,'{manual_research_conflicts}',COALESCE(website_provenance->'manual_research_conflicts','[]'::jsonb)||$2::jsonb,true) WHERE id=$1")
        .bind(school_id).bind(serde_json::to_value(conflicts)?).execute(connection).await?;
    Ok(())
}

fn conflicts(
    review: &Value,
    incoming: &School,
    source_url: &str,
    details: &Value,
) -> Result<Vec<Value>> {
    let identity = review
        .get("identity_snapshot")
        .context("manual website identity snapshot is missing")?;
    let mut conflicts = Vec::new();
    super::add_conflict(
        &mut conflicts,
        "name",
        identity.get("name"),
        Some(serde_json::json!(incoming.name)),
        source_url,
        details,
    );
    super::add_identity_conflict(
        &mut conflicts,
        "city",
        identity.get("city"),
        incoming.city.as_deref(),
        source_url,
        details,
    );
    super::add_identity_conflict(
        &mut conflicts,
        "state",
        identity.get("state"),
        incoming.state.as_deref(),
        source_url,
        details,
    );
    super::add_researched_field_conflict(
        &mut conflicts,
        "website",
        review.get("website"),
        incoming.website.as_deref(),
        source_url,
        details,
    );
    Ok(conflicts)
}
