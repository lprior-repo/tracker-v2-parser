use anyhow::{Context, Result};
use sqlx::types::Json;

use crate::{
    database::{Database, Job, keys},
    model::{PageKind, Target},
};

impl Database {
    #[tracing::instrument(skip(self, target))]
    pub(crate) async fn replay_checkpoint(&self, target: &Target) -> Result<Job> {
        let source = target.source.as_str();
        let checkpoint: Option<(i64, Json<Target>)> =
            sqlx::query_as("SELECT id,cursor FROM scrape_pages WHERE source=$1 AND page_key=$2")
                .bind(source)
                .bind(keys::target(target)?)
                .fetch_optional(&self.pool)
                .await
                .context("finding canonical replay checkpoint")?;
        let (id, Json(target)) = match checkpoint {
            Some(checkpoint) => checkpoint,
            None if target.kind == PageKind::Contacts => self.contact_checkpoint(target).await?,
            None => return Err(sqlx::Error::RowNotFound).context("finding replay checkpoint"),
        };
        Ok(Job { id, target })
    }

    async fn contact_checkpoint(&self, target: &Target) -> Result<(i64, Json<Target>)> {
        sqlx::query_as(
            "SELECT id,cursor FROM scrape_pages WHERE source=$1 AND url=$2 \
             AND cursor->>'kind'='contacts' AND cursor->'context'->>'school_id'=$3 \
             ORDER BY id LIMIT 1",
        )
        .bind(target.source.as_str())
        .bind(&target.url)
        .bind(target.context.school_id.as_deref())
        .fetch_one(&self.pool)
        .await
        .context("finding contact replay checkpoint")
    }
}
