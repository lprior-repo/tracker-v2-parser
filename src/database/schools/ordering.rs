use anyhow::Result;
use sqlx::PgConnection;

use crate::model::{School, Source};

#[tracing::instrument(skip_all)]
pub(super) async fn prepare<'a>(
    connection: &mut PgConnection,
    source: Source,
    schools: &'a [School],
) -> Result<Vec<&'a School>> {
    let mut ordered = schools.iter().collect::<Vec<_>>();
    if schools
        .iter()
        .any(|school| school.id.trim().is_empty() || school.name.trim().is_empty())
    {
        return Ok(ordered);
    }
    if ordered.is_empty() {
        return Ok(ordered);
    }
    let ids = ordered
        .iter()
        .map(|school| school.id.as_str())
        .collect::<Vec<_>>();
    let researched = sqlx::query_scalar::<_, bool>(
        "SELECT address_provenance IS NOT NULL OR website_provenance IS NOT NULL \
         FROM schools WHERE source=$1 AND source_id=ANY($2) ORDER BY id FOR UPDATE",
    )
    .bind(source.as_str())
    .bind(ids)
    .fetch_all(connection)
    .await?;
    if !researched.into_iter().any(std::convert::identity) {
        ordered.sort_by(|left, right| left.id.cmp(&right.id));
    }
    Ok(ordered)
}
