use crate::model::Source;
use anyhow::{Context, Result, ensure};
use serde_json::Value;

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

pub(super) async fn lock_school(
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

pub(super) fn verify_school(
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

pub(super) fn is_blank(value: Option<&String>) -> bool {
    value
        .map(String::as_str)
        .is_none_or(|current| current.trim().is_empty())
}
