use anyhow::{Context, Result};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{Coach, PageKind, Target};

use super::schools;

#[tracing::instrument(skip(connection, target, coaches))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    target: &Target,
    coaches: &[Coach],
    url: &str,
) -> Result<()> {
    if target.kind == PageKind::Contacts {
        mark_previous_claims(connection, target, url).await?;
        withdraw_unconfirmed_office(connection, target).await?;
    }
    stream::iter(coaches.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, coach| async move {
            save(connection, target, coach, url).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, target, coach))]
async fn save(
    connection: &mut PgConnection,
    target: &Target,
    coach: &Coach,
    url: &str,
) -> Result<()> {
    let school = schools::find(connection, target.source, Some(&coach.school_id))
        .await?
        .context("coach must reference an observed school")?;
    let raw = serde_json::to_value(coach)?;
    sqlx::query(
        "INSERT INTO contacts(school_id,kind,name,role,email,sport,source_url,effective_from,effective_until,raw,source_key) \
         VALUES($1,'coach',$2,$3,$4,$5,$6,$7,$8,$9 || jsonb_build_object('checkpoint_url',$10::text), \
         encode(sha256(convert_to(jsonb_build_array($6::text,$2::text,$3::text,$5::text,lower(btrim($4::text)))::text,'UTF8')),'hex')) \
         ON CONFLICT(school_id,kind,source_key) DO UPDATE SET \
         effective_from=COALESCE(EXCLUDED.effective_from,contacts.effective_from), \
         effective_until=COALESCE(EXCLUDED.effective_until,contacts.effective_until), \
         email=EXCLUDED.email,raw=CASE WHEN contacts.raw ? 'manual_research' \
         THEN jsonb_set(EXCLUDED.raw,'{manual_research}',contacts.raw->'manual_research',true) \
         ELSE EXCLUDED.raw END,last_checked=clock_timestamp()",
    ).bind(school).bind(&coach.name).bind(&coach.role).bind(&coach.email)
        .bind(coach.sport.map(crate::model::Sport::as_str)).bind(url)
        .bind(raw.get("effective_from").and_then(serde_json::Value::as_str))
        .bind(raw.get("effective_until").and_then(serde_json::Value::as_str)).bind(&raw)
        .bind(&target.url).execute(connection).await?;
    Ok(())
}

#[tracing::instrument(skip(connection, target))]
async fn mark_previous_claims(
    connection: &mut PgConnection,
    target: &Target,
    url: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE contacts c SET raw=jsonb_set(c.raw,'{review_notes}', \
         COALESCE(c.raw->'review_notes','[]'::jsonb) || jsonb_build_array($5::text)) \
         FROM schools s WHERE c.school_id=s.id AND s.source=$1 AND s.source_id=$2 \
         AND c.kind='coach' AND (c.source_url IN ($3,$4) OR c.raw->>'checkpoint_url'=$3) \
         AND NOT COALESCE(c.raw->'review_notes','[]'::jsonb) @> jsonb_build_array($5::text)",
    )
    .bind(target.source.as_str())
    .bind(target.context.school_id.as_deref())
    .bind(&target.url)
    .bind(url)
    .bind("Earlier contact source claim; not confirmed by latest parse")
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection, target))]
pub(super) async fn review_office(
    connection: &mut PgConnection,
    target: &Target,
    url: &str,
) -> Result<()> {
    if target.kind != PageKind::Contacts {
        return Ok(());
    }
    sqlx::query(
        "UPDATE contacts c SET raw=jsonb_set(c.raw,'{review_notes}', \
         COALESCE(c.raw->'review_notes','[]'::jsonb) || jsonb_build_array($5::text)) \
         FROM schools s WHERE c.school_id=s.id AND s.source=$1 AND s.source_id=$2 \
         AND c.kind='school_office' AND (c.source_url IN ($3,$4) OR c.raw->>'checkpoint_url'=$3) \
         AND NOT COALESCE(c.raw->'review_notes','[]'::jsonb) @> jsonb_build_array($5::text)",
    )
    .bind(target.source.as_str())
    .bind(target.context.school_id.as_deref())
    .bind(&target.url)
    .bind(url)
    .bind("Earlier contact source claim; not confirmed by latest parse")
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection, target))]
async fn withdraw_unconfirmed_office(connection: &mut PgConnection, target: &Target) -> Result<()> {
    sqlx::query(
        "UPDATE schools s SET public_email=NULL WHERE s.source=$1 AND s.source_id=$2 \
         AND EXISTS (SELECT 1 FROM contacts c WHERE c.school_id=s.id AND c.kind='school_office' \
         AND c.email=s.public_email AND COALESCE(c.raw->'review_notes','[]'::jsonb) @> jsonb_build_array($3::text)) \
         AND NOT EXISTS (SELECT 1 FROM contacts c WHERE c.school_id=s.id AND c.kind='school_office' \
         AND c.email=s.public_email AND NOT COALESCE(c.raw->'review_notes','[]'::jsonb) @> jsonb_build_array($3::text))",
    )
    .bind(target.source.as_str())
    .bind(target.context.school_id.as_deref())
    .bind("Earlier contact source claim; not confirmed by latest parse")
    .execute(connection)
    .await?;
    Ok(())
}
