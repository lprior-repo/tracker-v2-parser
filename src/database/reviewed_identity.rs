use anyhow::{Result, ensure};
use sqlx::PgConnection;

use crate::model::{Athlete, PageContext, PageKind, Source, Target};

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
    sqlx::query_scalar(
        "SELECT count(*)=2 AND count(DISTINCT p.athlete_id)=1 \
         AND count(DISTINCT p.details#>>'{_reviewed_identity,manifest_sha256}')=1 \
         AND COALESCE(bool_and(COALESCE( \
           p.details#>>'{_reviewed_identity,reviewed_by}'='user' \
           AND p.details#>>'{_reviewed_identity,canonical_athlete_id}'=p.athlete_id::text \
           AND jsonb_array_length(p.details#>'{_reviewed_identity,profiles}')=2 \
           AND p.details#>'{_reviewed_identity,profiles}' @> jsonb_build_array(jsonb_build_object('source',p.source,'source_id',p.source_id)) \
           AND p.details#>'{_reviewed_identity,accepted_names}' @> jsonb_build_array(lower(btrim(p.name))) \
         ,FALSE)),FALSE) FROM athlete_sources p WHERE p.athlete_id IN \
         (SELECT athlete_id FROM athlete_sources WHERE id=ANY($1))",
    ).bind(profiles).fetch_one(connection).await.map_err(Into::into)
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
    sqlx::query_scalar("SELECT details#>'{_reviewed_identity,accepted_names}' @> jsonb_build_array(lower(btrim($2::text))) FROM athlete_sources WHERE id=$1")
        .bind(profile).bind(name).fetch_one(connection).await.map_err(Into::into)
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
