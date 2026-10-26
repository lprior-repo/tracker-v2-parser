use anyhow::{Context, Result};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{Coach, Source};

use super::schools;

#[tracing::instrument(skip(connection, coaches))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    source: Source,
    coaches: &[Coach],
    url: &str,
) -> Result<()> {
    stream::iter(coaches.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, coach| async move {
            save(connection, source, coach, url).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, coach))]
async fn save(
    connection: &mut PgConnection,
    source: Source,
    coach: &Coach,
    url: &str,
) -> Result<()> {
    let school = schools::find(connection, source, Some(&coach.school_id))
        .await?
        .context("coach must reference an observed school")?;
    let raw = serde_json::to_value(coach)?;
    sqlx::query(
        "INSERT INTO contacts(school_id,kind,name,role,email,sport,source_url,effective_from,effective_until,raw) \
         VALUES($1,'coach',$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(school_id,kind,email_key,role) DO UPDATE SET \
         name=COALESCE(EXCLUDED.name,contacts.name),sport=COALESCE(EXCLUDED.sport,contacts.sport), \
         effective_from=COALESCE(EXCLUDED.effective_from,contacts.effective_from), \
         effective_until=COALESCE(EXCLUDED.effective_until,contacts.effective_until), \
         source_url=EXCLUDED.source_url,raw=EXCLUDED.raw,last_checked=clock_timestamp()",
    ).bind(school).bind(&coach.name).bind(&coach.role).bind(&coach.email)
        .bind(coach.sport.map(crate::model::Sport::as_str)).bind(url)
        .bind(raw.get("effective_from").and_then(serde_json::Value::as_str))
        .bind(raw.get("effective_until").and_then(serde_json::Value::as_str)).bind(&raw)
        .execute(connection).await?;
    Ok(())
}
