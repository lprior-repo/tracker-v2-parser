use anyhow::Result;
use sqlx::PgPool;

use crate::model::Counts;

#[tracing::instrument(skip(pool))]
pub(super) async fn counts(pool: &PgPool) -> Result<Counts> {
    let (schools, athletes, performances, coaches): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM schools),(SELECT COUNT(*) FROM athletes WHERE name IS NOT NULL), \
         (SELECT COUNT(*) FROM results),(SELECT COUNT(*) FROM contacts WHERE kind='coach')",
    ).fetch_one(pool).await?;
    let jobs: Vec<(String, i64)> =
        sqlx::query_as("SELECT status,COUNT(*) FROM scrape_pages GROUP BY status ORDER BY status")
            .fetch_all(pool)
            .await?;
    Ok(Counts {
        schools,
        athletes,
        performances,
        coaches,
        jobs: jobs.into_iter().collect(),
    })
}
