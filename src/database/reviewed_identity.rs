use anyhow::{Result, ensure};
use serde_json::Value;
use sqlx::PgConnection;

use crate::model::{Athlete, PageContext, PageKind, Source, Target};

#[derive(sqlx::FromRow)]
struct ReviewedProfile {
    athlete_id: i64,
    manifest_sha256: Option<String>,
    name: Option<String>,
    accepted_names: Option<Value>,
    valid: bool,
}

pub fn normalized(value: &str) -> String {
    value.trim().to_lowercase()
}

fn accepted(profile: &ReviewedProfile) -> bool {
    let Some(name) = profile.name.as_deref() else {
        return false;
    };
    let name = normalized(name);
    profile
        .accepted_names
        .as_ref()
        .and_then(Value::as_array)
        .is_some_and(|names| {
            names
                .iter()
                .any(|value| value.as_str() == Some(name.as_str()))
        })
}

pub fn validate_profile_url(source: Source, expected: &str, url: &str) -> Result<()> {
    let target = Target::get(
        source,
        PageKind::Athlete,
        url.to_owned(),
        PageContext::default(),
    );
    crate::transport::validate_target(&target)?;
    let (observed_source, observed) = match source {
        Source::MileSplit => (source, crate::sources::milesplit::profile_identity(url)?),
        Source::AthleticNet => super::keys::profile_link(url)?,
    };
    ensure!(
        observed_source == source && observed == expected,
        "published profile URL conflicts with reviewed source identity"
    );
    Ok(())
}

#[tracing::instrument(skip(connection, profiles))]
pub(super) async fn same_group(connection: &mut PgConnection, profiles: &[i64]) -> Result<bool> {
    let rows = sqlx::query_as::<_, ReviewedProfile>(
        "SELECT p.athlete_id,p.name, \
         p.details#>>'{_reviewed_identity,manifest_sha256}' AS manifest_sha256, \
         p.details#>'{_reviewed_identity,accepted_names}' AS accepted_names, \
         COALESCE( \
           p.details#>>'{_reviewed_identity,reviewed_by}'='user' \
           AND p.details#>>'{_reviewed_identity,canonical_athlete_id}'=p.athlete_id::text \
           AND jsonb_array_length(p.details#>'{_reviewed_identity,profiles}')=2 \
           AND p.details#>'{_reviewed_identity,profiles}' @> jsonb_build_array(jsonb_build_object('source',p.source,'source_id',p.source_id)) \
         ,FALSE) AS valid FROM athlete_sources p WHERE p.athlete_id IN \
         (SELECT athlete_id FROM athlete_sources WHERE id=ANY($1)) LIMIT 3",
    ).bind(profiles).fetch_all(connection).await?;
    let Some(first) = rows.first() else {
        return Ok(false);
    };
    Ok(rows.len() == 2
        && first.manifest_sha256.is_some()
        && rows.iter().all(|row| {
            row.valid
                && row.athlete_id == first.athlete_id
                && row.manifest_sha256 == first.manifest_sha256
                && accepted(row)
        }))
}

#[tracing::instrument(skip(connection, name))]
pub(super) async fn accepts_name(
    connection: &mut PgConnection,
    profile: i64,
    name: &str,
) -> Result<bool> {
    if !same_group(connection, &[profile]).await? {
        return Ok(false);
    }
    sqlx::query_scalar("SELECT details#>'{_reviewed_identity,accepted_names}' @> jsonb_build_array($2::text) FROM athlete_sources WHERE id=$1")
        .bind(profile).bind(normalized(name)).fetch_one(connection).await.map_err(Into::into)
}

#[tracing::instrument(skip(connection, observed))]
pub(super) async fn flag_fact_conflict(
    connection: &mut PgConnection,
    profile: i64,
    observed: &Athlete,
) -> Result<()> {
    let facts = serde_json::json!({"kind":"reviewed_person_with_conflicting_source_facts","name":observed.name,"state":observed.state,"gender":observed.gender,"graduation_year":observed.graduation_year,"school_id":observed.school_id,"profile_url":observed.profile_url});
    sqlx::query("UPDATE athletes SET needs_review=TRUE WHERE id=(SELECT athlete_id FROM athlete_sources WHERE id=$1)")
        .bind(profile).execute(&mut *connection).await?;
    sqlx::query("UPDATE athlete_sources p SET details=jsonb_set(p.details,'{_reviewed_identity,latest_fact_conflicts}',COALESCE(p.details#>'{_reviewed_identity,latest_fact_conflicts}','{}'::jsonb) || jsonb_build_object(observed.source || ':' || observed.source_id,$2::jsonb)) FROM athlete_sources observed WHERE observed.id=$1 AND p.athlete_id=observed.athlete_id")
        .bind(profile).bind(facts).execute(connection).await?;
    Ok(())
}
