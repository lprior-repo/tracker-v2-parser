use anyhow::{Context, Result, bail};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{Athlete, Source};

use super::{keys, reviewed_identity, schools};

#[tracing::instrument(skip(connection, observed))]
pub(super) async fn review_school_conflicts(
    connection: &mut PgConnection,
    source: Source,
    observed: &[crate::model::School],
) -> Result<()> {
    if observed.is_empty() {
        return Ok(());
    }
    let ids = observed
        .iter()
        .map(|school| school.id.as_str())
        .collect::<Vec<_>>();
    sqlx::query(
        "UPDATE athletes a SET needs_review=TRUE WHERE EXISTS ( \
         SELECT 1 FROM athlete_sources p JOIN schools s ON s.id=p.school_id \
         JOIN athlete_sources other ON other.athlete_id=p.athlete_id AND other.id<>p.id \
         JOIN schools other_school ON other_school.id=other.school_id \
         WHERE p.athlete_id=a.id AND s.source=$1 AND s.source_id=ANY($2) \
         AND NULLIF(lower(btrim(s.state)),'') IS NOT NULL \
         AND NULLIF(lower(btrim(other_school.state)),'') IS NOT NULL \
         AND lower(btrim(s.state))<>lower(btrim(other_school.state)))",
    )
    .bind(source.as_str())
    .bind(ids)
    .execute(connection)
    .await?;
    Ok(())
}

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
    let reviewed_name = reviewed_identity::accepts_name(connection, profile, &athlete.name).await?;
    let separated =
        separate_conflicting_profile(connection, profile, source, athlete, reviewed_name).await?;
    update(
        connection,
        profile,
        source,
        athlete,
        reviewed_name && !separated,
    )
    .await?;
    save_links(connection, profile, &athlete.same_as, separated).await
}

#[tracing::instrument(skip(connection, athlete))]
async fn separate_conflicting_profile(
    connection: &mut PgConnection,
    profile: i64,
    source: Source,
    athlete: &Athlete,
    reviewed_name: bool,
) -> Result<bool> {
    let school = schools::find(connection, source, athlete.school_id.as_deref()).await?;
    let conflict: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM athlete_sources p \
         JOIN athlete_sources other ON other.athlete_id=p.athlete_id AND other.id<>p.id \
         LEFT JOIN schools incoming_school ON incoming_school.id=$3 \
         LEFT JOIN schools other_school ON other_school.id=other.school_id \
         WHERE p.id=$1 AND ( \
         (NOT $7 AND COALESCE(lower(btrim(other.name))<>lower(btrim($2)),FALSE)) OR \
         COALESCE(lower(btrim(other.state))<>lower(btrim($4)),FALSE) OR \
         COALESCE(lower(btrim(other.gender))<>lower(btrim($5)),FALSE) OR \
         COALESCE(other.graduation_year<>$6,FALSE) OR \
         COALESCE(lower(btrim(other_school.name))<>lower(btrim(incoming_school.name)),FALSE) OR \
         COALESCE(NULLIF(lower(btrim(other_school.state)),'')<>NULLIF(lower(btrim(incoming_school.state)),''),FALSE)))",
    )
    .bind(profile)
    .bind(&athlete.name)
    .bind(school)
    .bind(&athlete.state)
    .bind(&athlete.gender)
    .bind(athlete.graduation_year)
    .bind(reviewed_name)
    .fetch_one(&mut *connection)
    .await?;
    if !conflict {
        return Ok(false);
    }
    if reviewed_name {
        reviewed_identity::flag_fact_conflict(connection, profile, athlete).await?;
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
    reviewed_name: bool,
) -> Result<()> {
    let school = schools::find(connection, source, athlete.school_id.as_deref()).await?;
    let current_affiliation = source == Source::MileSplit
        && athlete
            .source_values
            .as_ref()
            .is_some_and(|values| values.get("current_affiliation").is_some());
    let details =
        affiliation_details(connection, profile, school, athlete, current_affiliation).await?;
    sqlx::query(
        "UPDATE athletes a SET \
         needs_review=a.needs_review OR $8 \
         OR (NOT $10 AND COALESCE(lower(btrim(p.name)) <> lower(btrim($2)), FALSE)) \
         OR COALESCE(p.school_id <> $3, FALSE) \
         OR COALESCE(lower(btrim(p.state)) <> lower(btrim($4)), FALSE) \
         OR COALESCE(lower(btrim(p.gender)) <> lower(btrim($5)), FALSE) \
         OR COALESCE(p.graduation_year <> $7, FALSE) \
         OR (NOT $10 AND COALESCE(lower(btrim(a.name)) <> lower(btrim($2)), FALSE)) \
         OR COALESCE(lower(btrim(a.state)) <> lower(btrim($4)), FALSE) \
         OR COALESCE(lower(btrim(a.gender)) <> lower(btrim($5)), FALSE) \
         OR COALESCE(a.graduation_year <> $7, FALSE), \
         name=COALESCE(a.name,$2),school_id=CASE WHEN $9 THEN $3 WHEN p.details->'source_values' ? 'current_affiliation' THEN p.school_id ELSE COALESCE(a.school_id,$3) END, \
         state=CASE WHEN $9 THEN $4 WHEN p.details->'source_values' ? 'current_affiliation' THEN p.state ELSE COALESCE(a.state,$4) END, \
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
    .bind(current_affiliation)
    .bind(reviewed_name)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "UPDATE athlete_sources SET name=COALESCE(name,$2), \
         school_id=CASE WHEN $11 THEN $3 WHEN details->'source_values' ? 'current_affiliation' THEN school_id ELSE COALESCE(school_id,$3) END, \
         state=CASE WHEN $11 THEN $4 WHEN details->'source_values' ? 'current_affiliation' THEN state ELSE COALESCE(state,$4) END,gender=COALESCE(gender,$5),grade=COALESCE($6,grade), \
         graduation_year=COALESCE(graduation_year,$7),profile_url=COALESCE(profile_url,$8), \
         details=(CASE WHEN $11 THEN details - 'school_id' - 'state' ELSE details END) || (jsonb_strip_nulls($9::jsonb) - 'same_as' - 'source_values') || \
         CASE WHEN $9::jsonb ? 'source_values' THEN jsonb_build_object('source_values', \
         COALESCE(details->'source_values','{}'::jsonb) || ($9::jsonb->'source_values') || \
         jsonb_build_object('pages', COALESCE(details->'source_values'->'pages','{}'::jsonb) || \
         COALESCE($9::jsonb->'source_values'->'pages','{}'::jsonb))) ELSE '{}'::jsonb END || \
         jsonb_build_object('needs_review', COALESCE((details->>'needs_review')::boolean,FALSE) OR $10, \
         '_identity_conflict', COALESCE((details->>'_identity_conflict')::boolean,FALSE) OR \
         (NOT $12 AND COALESCE(lower(btrim(name))<>lower(btrim($2)),FALSE)) OR \
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
    .bind(details)
    .bind(athlete.needs_review)
    .bind(current_affiliation)
    .bind(reviewed_name)
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection, athlete))]
async fn affiliation_details(
    connection: &mut PgConnection,
    profile: i64,
    school: Option<i64>,
    athlete: &Athlete,
    current: bool,
) -> Result<serde_json::Value> {
    let mut details = serde_json::to_value(athlete)?;
    if !current {
        return Ok(details);
    }
    let mut uncertainties: serde_json::Value = sqlx::query_scalar(
        "SELECT COALESCE(details#>'{source_values,school_state_uncertainties}','{}'::jsonb) FROM athlete_sources WHERE id=$1",
    )
    .bind(profile)
    .fetch_one(&mut *connection)
    .await?;
    let uncertain = if athlete.state.is_none() && school.is_some() {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT CASE WHEN COALESCE(details#>'{source_values,school_state_uncertainties}'->>$3=(SELECT s.state FROM schools s WHERE s.id=$2) OR ((school_id=$2 OR details->>'school_id'=$3) AND ( \
             (COALESCE((details#>>'{source_values,current_affiliation,school_state_unverified}')::boolean,FALSE) \
             AND details#>>'{source_values,current_affiliation,unverified_school_state}'=(SELECT s.state FROM schools s WHERE s.id=$2)) OR \
             (NOT COALESCE(details#>'{source_values,current_affiliation}' ? 'school_state_unverified',FALSE) \
             AND ((state IS NOT NULL AND state=(SELECT s.state FROM schools s WHERE s.id=$2)) \
             OR details->'source_values' ? 'current_affiliation')))),FALSE) \
             THEN (SELECT s.state FROM schools s WHERE s.id=$2) ELSE NULL END \
             FROM athlete_sources WHERE id=$1",
        ).bind(profile).bind(school).bind(&athlete.school_id).fetch_one(connection).await?
    } else {
        None
    };
    let states = uncertainties
        .as_object_mut()
        .context("school state uncertainty evidence must be an object")?;
    if let Some(id) = &athlete.school_id {
        if let Some(state) = &uncertain {
            states.insert(id.clone(), serde_json::json!(state));
        } else if athlete.state.is_some() {
            states.remove(id);
        }
    }
    details
        .get_mut("source_values")
        .and_then(serde_json::Value::as_object_mut)
        .context("profile evidence must be an object")?
        .insert("school_state_uncertainties".to_owned(), uncertainties);
    let affiliation = details
        .pointer_mut("/source_values/current_affiliation")
        .and_then(serde_json::Value::as_object_mut)
        .context("current affiliation evidence must be an object")?;
    affiliation.insert(
        "school_state_unverified".to_owned(),
        serde_json::json!(uncertain.is_some()),
    );
    affiliation.insert(
        "unverified_school_state".to_owned(),
        serde_json::json!(uncertain),
    );
    Ok(details)
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
    if !separated && reviewed_identity::same_group(connection, &profiles).await? {
        return Ok(());
    }
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
         OR COUNT(DISTINCT NULLIF(lower(btrim(s.name)),''))>1 \
         OR COUNT(DISTINCT NULLIF(lower(btrim(s.state)),''))>1) OR \
         EXISTS(SELECT 1 FROM athlete_sources p \
         LEFT JOIN schools s ON s.id=p.school_id \
         LEFT JOIN schools observed ON observed.source=p.source AND observed.source_id=p.details->>'school_id' \
         WHERE p.athlete_id IN (SELECT athlete_id FROM candidates) AND ( \
         COALESCE((p.details->>'_identity_conflict')::boolean,FALSE) OR \
         COALESCE(lower(btrim(p.name))<>lower(btrim(p.details->>'name')),FALSE) OR \
         COALESCE(lower(btrim(p.state))<>lower(btrim(p.details->>'state')),FALSE) OR \
         COALESCE(lower(btrim(p.gender))<>lower(btrim(p.details->>'gender')),FALSE) OR \
         COALESCE(p.graduation_year::text<>p.details->>'graduation_year',FALSE) OR \
         COALESCE(lower(btrim(s.name))<>lower(btrim(observed.name)),FALSE) OR \
         COALESCE(NULLIF(lower(btrim(s.state)),'')<>NULLIF(lower(btrim(observed.state)),''),FALSE)))",
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
