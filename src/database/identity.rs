use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{Athlete, Source};

use super::{keys, schools};

#[tracing::instrument(skip(connection, athletes))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    source: Source,
    athletes: &[Athlete],
) -> Result<()> {
    stream::iter(athletes.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, athlete| async move {
            save(connection, source, athlete).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, athlete))]
async fn save(connection: &mut PgConnection, source: Source, athlete: &Athlete) -> Result<()> {
    if athlete.name.trim().is_empty() {
        bail!(
            "observed {} athlete {} has no published name",
            source.as_str(),
            athlete.id
        );
    }
    let profile = reserve(
        connection,
        source,
        &athlete.id,
        athlete.profile_url.as_deref(),
    )
    .await?;
    let separated = separate_conflicting_profile(connection, profile, source, athlete).await?;
    update(connection, profile, source, athlete).await?;
    save_links(connection, profile, &athlete.same_as, separated).await
}

#[tracing::instrument(skip(connection, athlete))]
async fn separate_conflicting_profile(
    connection: &mut PgConnection,
    profile: i64,
    source: Source,
    athlete: &Athlete,
) -> Result<bool> {
    let school = schools::find(connection, source, athlete.school_id.as_deref()).await?;
    let conflict: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM athlete_sources p \
         JOIN athlete_sources other ON other.athlete_id=p.athlete_id AND other.id<>p.id \
         LEFT JOIN schools incoming_school ON incoming_school.id=$3 \
         LEFT JOIN schools other_school ON other_school.id=other.school_id \
         WHERE p.id=$1 AND ( \
         COALESCE(lower(btrim(other.name))<>lower(btrim($2)),FALSE) OR \
         COALESCE(lower(btrim(other.state))<>lower(btrim($4)),FALSE) OR \
         COALESCE(lower(btrim(other.gender))<>lower(btrim($5)),FALSE) OR \
         COALESCE(other.graduation_year<>$6,FALSE) OR \
         COALESCE(lower(btrim(other_school.name))<>lower(btrim(incoming_school.name)),FALSE)))",
    )
    .bind(profile)
    .bind(&athlete.name)
    .bind(school)
    .bind(&athlete.state)
    .bind(&athlete.gender)
    .bind(athlete.graduation_year)
    .fetch_one(&mut *connection)
    .await?;
    if !conflict {
        return Ok(false);
    }
    let previous: i64 = sqlx::query_scalar("SELECT athlete_id FROM athlete_sources WHERE id=$1")
        .bind(profile)
        .fetch_one(&mut *connection)
        .await?;
    let separate: i64 = sqlx::query_scalar(
        "INSERT INTO athletes(name,school_id,state,gender,grade,graduation_year,needs_review) \
         SELECT name,school_id,state,gender,grade,graduation_year,TRUE \
         FROM athlete_sources WHERE id=$1 RETURNING id",
    )
    .bind(profile)
    .fetch_one(&mut *connection)
    .await?;
    sqlx::query("UPDATE athlete_sources SET athlete_id=$2 WHERE id=$1")
        .bind(profile)
        .bind(separate)
        .execute(&mut *connection)
        .await?;
    sqlx::query("UPDATE results SET athlete_id=$2 WHERE athlete_source_id=$1")
        .bind(profile)
        .bind(separate)
        .execute(&mut *connection)
        .await?;
    sqlx::query(
        "UPDATE athletes a SET name=p.name,school_id=p.school_id,state=p.state,gender=p.gender, \
         grade=p.grade,graduation_year=p.graduation_year,needs_review=TRUE \
         FROM athlete_sources p WHERE a.id=$1 AND p.athlete_id=a.id",
    )
    .bind(previous)
    .execute(connection)
    .await?;
    Ok(true)
}

#[tracing::instrument(skip(connection, athlete))]
async fn update(
    connection: &mut PgConnection,
    profile: i64,
    source: Source,
    athlete: &Athlete,
) -> Result<()> {
    let school = schools::find(connection, source, athlete.school_id.as_deref()).await?;
    sqlx::query(
        "UPDATE athletes a SET \
         needs_review=a.needs_review OR $8 \
         OR COALESCE(lower(btrim(p.name)) <> lower(btrim($2)), FALSE) \
         OR COALESCE(p.school_id <> $3, FALSE) \
         OR COALESCE(lower(btrim(p.state)) <> lower(btrim($4)), FALSE) \
         OR COALESCE(lower(btrim(p.gender)) <> lower(btrim($5)), FALSE) \
         OR COALESCE(p.graduation_year <> $7, FALSE) \
         OR COALESCE(lower(btrim(a.name)) <> lower(btrim($2)), FALSE) \
         OR COALESCE(lower(btrim(a.state)) <> lower(btrim($4)), FALSE) \
         OR COALESCE(lower(btrim(a.gender)) <> lower(btrim($5)), FALSE) \
         OR COALESCE(a.graduation_year <> $7, FALSE), \
         name=COALESCE(a.name,$2),school_id=COALESCE(a.school_id,$3),state=COALESCE(a.state,$4), \
         gender=COALESCE(a.gender,$5),grade=COALESCE($6,a.grade), \
         graduation_year=COALESCE(a.graduation_year,$7) \
         FROM athlete_sources p WHERE p.id=$1 AND a.id=p.athlete_id",
    )
    .bind(profile)
    .bind(&athlete.name)
    .bind(school)
    .bind(&athlete.state)
    .bind(&athlete.gender)
    .bind(&athlete.grade)
    .bind(athlete.graduation_year)
    .bind(athlete.needs_review)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "UPDATE athlete_sources SET name=COALESCE(name,$2),school_id=COALESCE(school_id,$3), \
         state=COALESCE(state,$4),gender=COALESCE(gender,$5),grade=COALESCE($6,grade), \
         graduation_year=COALESCE(graduation_year,$7),profile_url=COALESCE(profile_url,$8), \
         details=details || (jsonb_strip_nulls($9::jsonb) - 'same_as' - 'source_values') || \
         CASE WHEN $9::jsonb ? 'source_values' THEN jsonb_build_object('source_values', \
         COALESCE(details->'source_values','{}'::jsonb) || ($9::jsonb->'source_values') || \
         jsonb_build_object('pages', COALESCE(details->'source_values'->'pages','{}'::jsonb) || \
         COALESCE($9::jsonb->'source_values'->'pages','{}'::jsonb))) ELSE '{}'::jsonb END || \
         jsonb_build_object('needs_review', COALESCE((details->>'needs_review')::boolean,FALSE) OR $10, \
         '_identity_conflict', COALESCE((details->>'_identity_conflict')::boolean,FALSE) OR \
         COALESCE(lower(btrim(name))<>lower(btrim($2)),FALSE) OR \
         COALESCE(school_id<>$3,FALSE) OR COALESCE(lower(btrim(state))<>lower(btrim($4)),FALSE) OR \
         COALESCE(lower(btrim(gender))<>lower(btrim($5)),FALSE) OR \
         COALESCE(graduation_year<>$7,FALSE), \
         'same_as', (SELECT COALESCE(jsonb_agg(DISTINCT link), '[]'::jsonb) \
         FROM jsonb_array_elements(COALESCE(details->'same_as','[]'::jsonb) || \
         COALESCE($9::jsonb->'same_as','[]'::jsonb)) AS links(link))), \
         last_seen=clock_timestamp() WHERE id=$1",
    )
    .bind(profile)
    .bind(&athlete.name)
    .bind(school)
    .bind(&athlete.state)
    .bind(&athlete.gender)
    .bind(&athlete.grade)
    .bind(athlete.graduation_year)
    .bind(&athlete.profile_url)
    .bind(serde_json::to_value(athlete)?)
    .bind(athlete.needs_review)
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection))]
async fn reserve(
    connection: &mut PgConnection,
    source: Source,
    source_id: &str,
    url: Option<&str>,
) -> Result<i64> {
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM athlete_sources WHERE source=$1 AND source_id=$2 FOR UPDATE",
    )
    .bind(source.as_str())
    .bind(source_id)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let athlete: i64 = sqlx::query_scalar("INSERT INTO athletes DEFAULT VALUES RETURNING id")
        .fetch_one(&mut *connection)
        .await?;
    sqlx::query_scalar(
        "INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url) VALUES($1,$2,$3,$4) RETURNING id",
    ).bind(athlete).bind(source.as_str()).bind(source_id).bind(url)
        .fetch_one(connection).await.map_err(Into::into)
}

#[tracing::instrument(skip(connection, links))]
async fn save_links(
    connection: &mut PgConnection,
    profile: i64,
    links: &[String],
    separated: bool,
) -> Result<()> {
    let (connection, profiles) = reserve_links(connection, profile, links).await?;
    let conflict: bool = separated || sqlx::query_scalar(
        "WITH candidates AS (SELECT athlete_id FROM athlete_sources WHERE id=ANY($1)) \
         SELECT EXISTS(SELECT source FROM athlete_sources WHERE athlete_id IN \
         (SELECT athlete_id FROM candidates) GROUP BY source HAVING COUNT(*)>1) OR \
         EXISTS(SELECT 1 FROM athlete_sources p LEFT JOIN schools s ON s.id=p.school_id \
         WHERE p.athlete_id IN (SELECT athlete_id FROM candidates) \
         HAVING COUNT(DISTINCT p.graduation_year)>1 \
         OR COUNT(DISTINCT NULLIF(lower(btrim(p.name)),''))>1 \
         OR COUNT(DISTINCT NULLIF(lower(btrim(p.gender)),''))>1 \
         OR COUNT(DISTINCT NULLIF(lower(btrim(p.state)),''))>1 \
         OR COUNT(DISTINCT NULLIF(lower(btrim(s.name)),''))>1) OR \
         EXISTS(SELECT 1 FROM athlete_sources p \
         LEFT JOIN schools s ON s.id=p.school_id \
         LEFT JOIN schools observed ON observed.source=p.source AND observed.source_id=p.details->>'school_id' \
         WHERE p.athlete_id IN (SELECT athlete_id FROM candidates) AND ( \
         COALESCE((p.details->>'_identity_conflict')::boolean,FALSE) OR \
         COALESCE(lower(btrim(p.name))<>lower(btrim(p.details->>'name')),FALSE) OR \
         COALESCE(lower(btrim(p.state))<>lower(btrim(p.details->>'state')),FALSE) OR \
         COALESCE(lower(btrim(p.gender))<>lower(btrim(p.details->>'gender')),FALSE) OR \
         COALESCE(p.graduation_year::text<>p.details->>'graduation_year',FALSE) OR \
         COALESCE(lower(btrim(s.name))<>lower(btrim(observed.name)),FALSE)))",
    )
    .bind(&profiles)
    .fetch_one(&mut *connection)
    .await?;
    if conflict {
        sqlx::query(
            "UPDATE athletes SET needs_review=TRUE WHERE id IN \
             (SELECT athlete_id FROM athlete_sources WHERE id=ANY($1))",
        )
        .bind(&profiles)
        .execute(connection)
        .await?;
        return Ok(());
    }
    stream::iter(profiles.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, referenced| async move {
            merge(connection, profile, referenced).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, links))]
async fn reserve_links<'a>(
    connection: &'a mut PgConnection,
    profile: i64,
    links: &[String],
) -> Result<(&'a mut PgConnection, Vec<i64>)> {
    stream::iter(links.iter().map(Ok::<_, anyhow::Error>))
        .try_fold((connection, vec![profile]), |(connection, mut profiles), link| async move {
            match keys::profile_link(link) {
                Ok((source, source_id)) => profiles.push(reserve(connection, source, &source_id, Some(link)).await?),
                Err(error) => {
                    tracing::warn!(%error, profile, "flagging unsupported explicit athlete link for review");
                    sqlx::query("UPDATE athletes SET needs_review=TRUE WHERE id=(SELECT athlete_id FROM athlete_sources WHERE id=$1)")
                        .bind(profile).execute(&mut *connection).await?;
                }
            }
            Ok((connection, profiles))
        }).await
}

#[tracing::instrument(skip(connection))]
async fn merge(connection: &mut PgConnection, first: i64, second: i64) -> Result<()> {
    let (left, right): (i64, i64) = sqlx::query_as(
        "SELECT a.athlete_id,b.athlete_id FROM athlete_sources a CROSS JOIN athlete_sources b WHERE a.id=$1 AND b.id=$2",
    ).bind(first).bind(second).fetch_one(&mut *connection).await?;
    if left == right {
        return Ok(());
    }
    move_identity(connection, left.min(right), left.max(right)).await
}

#[tracing::instrument(skip(connection))]
async fn move_identity(connection: &mut PgConnection, keep: i64, remove: i64) -> Result<()> {
    sqlx::query(
        "UPDATE athletes a SET name=COALESCE(a.name,b.name),gender=COALESCE(a.gender,b.gender), \
         grade=COALESCE(a.grade,b.grade),graduation_year=COALESCE(a.graduation_year,b.graduation_year), \
         school_id=COALESCE(a.school_id,b.school_id),state=COALESCE(a.state,b.state), \
         needs_review=a.needs_review OR b.needs_review FROM athletes b WHERE a.id=$1 AND b.id=$2",
    ).bind(keep).bind(remove).execute(&mut *connection).await?;
    sqlx::query(
        "WITH moved AS (UPDATE results SET athlete_id=$1 WHERE athlete_id=$2) \
        UPDATE athlete_sources SET athlete_id=$1 WHERE athlete_id=$2",
    )
    .bind(keep)
    .bind(remove)
    .execute(&mut *connection)
    .await?;
    sqlx::query("DELETE FROM athletes WHERE id=$1")
        .bind(remove)
        .execute(connection)
        .await?;
    Ok(())
}

#[tracing::instrument(skip(connection))]
pub(super) async fn find(
    connection: &mut PgConnection,
    source: Source,
    source_id: &str,
) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT id FROM athlete_sources WHERE source=$1 AND source_id=$2 AND name IS NOT NULL",
    )
    .bind(source.as_str())
    .bind(source_id)
    .fetch_optional(connection)
    .await?
    .with_context(|| {
        format!(
            "unknown {} athlete ID {source_id}; import its published profile or named result first",
            source.as_str()
        )
    })
}
