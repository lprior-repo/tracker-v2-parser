use anyhow::{Context, Result};
use futures::TryStreamExt;
use sqlx::PgPool;

use crate::{
    model::{School, Source},
    sources,
};

use super::queue;

#[tracing::instrument(skip(pool, states, sources))]
pub(super) async fn backfill(
    pool: &PgPool,
    states: &[String],
    sources: &[Source],
    season: i32,
) -> Result<u64> {
    let enabled: Vec<_> = sources.iter().copied().map(Source::as_str).collect();
    let rows = sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT source,jsonb_build_object('id',source_id,'name',name,'state',state,'website',website) \
         FROM schools WHERE source=ANY($1) AND state=ANY($2) \
         AND website IS NOT NULL AND btrim(website) <> '' ORDER BY id",
    )
    .bind(&enabled)
    .bind(states)
    .fetch(pool)
    .map_err(anyhow::Error::from);
    let transaction = pool.begin().await?;
    let result = rows.try_fold((transaction, 0_u64), |(mut transaction, count), (source, value)| async move {
        let source: Source = serde_json::from_value(serde_json::Value::String(source))?;
        let school: School = serde_json::from_value(value)?;
        let targets = match sources::school_contacts(source, &school, season) {
            Ok(targets) => targets,
            Err(error) => {
                tracing::warn!(school_id = %school.id, "contact discovery skipped: {error:#}");
                return Ok((transaction, count));
            }
        };
        let changed = queue::enqueue_rows(&mut transaction, &targets, false).await?;
        Ok((transaction, count.checked_add(changed).context("contact checkpoint count overflow")?))
    }).await;
    match result {
        Ok((mut transaction, count)) => {
            queue::note_contact_stops(&mut transaction).await?;
            transaction
                .commit()
                .await
                .context("committing contact website backfill")?;
            Ok(count)
        }
        Err(error) => Err(error).context("backfilling official school contact websites"),
    }
}
