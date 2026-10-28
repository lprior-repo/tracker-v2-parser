use anyhow::{Result, ensure};

use super::Database;

impl Database {
    #[tracing::instrument(skip(self))]
    pub(crate) async fn exclude_unattempted(&self, id: i64, reason: &str) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE scrape_pages SET status='blocked', \
             cursor=jsonb_set(cursor,'{public_results_exclusion}',to_jsonb($3::text)), \
             notes=array_append(notes,$2),last_error=COALESCE(last_error,$2) \
             WHERE id=$1 AND status IN ('pending','failed') AND attempts=0",
        )
        .bind(id)
        .bind(reason)
        .bind(crate::acquisition::EXCLUSION)
        .execute(self.pool())
        .await?
        .rows_affected();
        ensure!(
            changed == 1,
            "previously attempted excluded checkpoint requires human review; state was preserved"
        );
        Ok(())
    }
}
