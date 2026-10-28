use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use super::{
    Report,
    manifest::{Reviewed, normalized},
};
use crate::database::reviewed_identity;

#[derive(sqlx::FromRow, Serialize)]
struct Existing {
    profile_id: i64,
    athlete_id: i64,
    source: String,
    source_id: String,
    name: Option<String>,
    canonical_name: Option<String>,
    profile_url: Option<String>,
    school_id: Option<i64>,
    canonical_school_id: Option<i64>,
    state: Option<String>,
    canonical_state: Option<String>,
    gender: Option<String>,
    canonical_gender: Option<String>,
    graduation_year: Option<i32>,
    canonical_graduation_year: Option<i32>,
    latest_name: Option<String>,
    latest_state: Option<String>,
    latest_gender: Option<String>,
    latest_graduation_year: Option<i32>,
    school_source_id: Option<String>,
    latest_school_source_id: Option<String>,
    review: Option<Value>,
}

async fn existing(connection: &mut PgConnection, reviewed: &Reviewed) -> Result<Vec<Existing>> {
    let [left, right] = &reviewed.manifest.profiles;
    let rows = sqlx::query_as(
        "SELECT p.id profile_id,p.athlete_id,p.source,p.source_id,p.name,a.name canonical_name, \
         p.profile_url,p.school_id,a.school_id canonical_school_id,p.state,a.state canonical_state, \
         p.gender,a.gender canonical_gender,p.graduation_year,a.graduation_year canonical_graduation_year, \
         p.details->>'name' latest_name,p.details->>'state' latest_state,p.details->>'gender' latest_gender, \
         (p.details->>'graduation_year')::integer latest_graduation_year,s.source_id school_source_id, \
         p.details->>'school_id' latest_school_source_id,p.details->'_reviewed_identity' review \
         FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id LEFT JOIN schools s ON s.id=p.school_id \
         WHERE (p.source=$1 AND p.source_id=$2) OR (p.source=$3 AND p.source_id=$4) \
         ORDER BY p.id FOR UPDATE OF p,a",
    ).bind(left.source.as_str()).bind(&left.source_id)
        .bind(right.source.as_str()).bind(&right.source_id).fetch_all(connection).await?;
    Ok(rows)
}

fn consistent<'a>(values: impl Iterator<Item = Option<&'a str>>) -> bool {
    values
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .map(normalized)
        .collect::<BTreeSet<_>>()
        .len()
        <= 1
}

fn validate(rows: &[Existing], reviewed: &Reviewed) -> Result<()> {
    let manifest = &reviewed.manifest;
    ensure!(
        rows.len() == 2,
        "reviewed identity requires two existing source profiles"
    );
    rows.iter().try_for_each(|row| {
        let expected = manifest
            .profiles
            .iter()
            .find(|profile| {
                profile.source.as_str() == row.source && profile.source_id == row.source_id
            })
            .context("source identity differs from reviewed manifest")?;
        let name = row
            .name
            .as_deref()
            .context("reviewed source profile must have an observed name")?;
        ensure!(
            manifest.accepts(name)
                && row
                    .canonical_name
                    .as_deref()
                    .is_none_or(|name| manifest.accepts(name))
                && row
                    .latest_name
                    .as_deref()
                    .is_none_or(|name| manifest.accepts(name)),
            "observed identity name was not explicitly accepted by the review"
        );
        reviewed_identity::validate_profile_url(
            expected.source,
            &row.source_id,
            row.profile_url
                .as_deref()
                .context("reviewed source profile must have a published URL")?,
        )
    })?;
    validate_facts(rows)
}

fn validate_facts(rows: &[Existing]) -> Result<()> {
    ensure!(
        consistent(
            rows.iter()
                .flat_map(|row| [row.state.as_deref(), row.canonical_state.as_deref()])
        ),
        "reviewed profiles have conflicting states"
    );
    ensure!(
        consistent(
            rows.iter()
                .flat_map(|row| [row.gender.as_deref(), row.canonical_gender.as_deref()])
        ),
        "reviewed profiles have conflicting genders"
    );
    ensure!(
        rows.iter()
            .flat_map(|row| [row.graduation_year, row.canonical_graduation_year])
            .flatten()
            .collect::<BTreeSet<_>>()
            .len()
            <= 1,
        "reviewed profiles have conflicting graduation years"
    );
    ensure!(
        rows.iter()
            .flat_map(|row| [row.school_id, row.canonical_school_id])
            .flatten()
            .collect::<BTreeSet<_>>()
            .len()
            <= 1,
        "reviewed profiles have conflicting schools"
    );
    Ok(())
}

fn fact_warnings(rows: &[Existing]) -> Vec<Value> {
    rows.iter().filter_map(|row| {
        let conflict = !consistent([row.state.as_deref(),row.latest_state.as_deref()].into_iter())
            || !consistent([row.gender.as_deref(),row.latest_gender.as_deref()].into_iter())
            || row.graduation_year.zip(row.latest_graduation_year).is_some_and(|(old,new)| old != new)
            || !consistent([row.school_source_id.as_deref(),row.latest_school_source_id.as_deref()].into_iter());
        conflict.then(|| json!({"source":row.source,"source_id":row.source_id,"kind":"retained_and_latest_source_facts_disagree","retained":{"state":row.state,"gender":row.gender,"graduation_year":row.graduation_year,"school_id":row.school_source_id},"latest":{"state":row.latest_state,"gender":row.latest_gender,"graduation_year":row.latest_graduation_year,"school_id":row.latest_school_source_id}}))
    }).collect()
}

fn same_review(row: &Existing, reviewed: &Reviewed) -> bool {
    row.athlete_id == reviewed.manifest.canonical_athlete_id
        && row
            .review
            .as_ref()
            .and_then(|value| value.get("manifest_sha256"))
            .and_then(Value::as_str)
            == Some(reviewed.sha256.as_str())
}

pub(super) async fn apply(connection: &mut PgConnection, reviewed: &Reviewed) -> Result<Report> {
    let rows = existing(connection, reviewed).await?;
    validate(&rows, reviewed)?;
    let ids = rows.iter().map(|row| row.profile_id).collect::<Vec<_>>();
    let owners = rows.iter().map(|row| row.athlete_id).collect::<Vec<_>>();
    let group_size: i64 =
        sqlx::query_scalar("SELECT count(*) FROM athlete_sources WHERE athlete_id=ANY($1)")
            .bind(&owners)
            .fetch_one(&mut *connection)
            .await?;
    ensure!(
        group_size == 2,
        "reviewed link cannot absorb any unreviewed third profile"
    );
    let already_applied = rows.iter().all(|row| same_review(row, reviewed));
    if !already_applied {
        ensure!(
            rows.iter().all(|row| row.review.is_none()),
            "source profile already belongs to a different reviewed identity decision"
        );
        ensure!(
            rows.iter().all(
                |row| reviewed
                    .manifest
                    .profiles
                    .iter()
                    .any(|profile| profile.source.as_str() == row.source
                        && profile.source_id == row.source_id
                        && profile.expected_athlete_id == row.athlete_id)
            ),
            "reviewed identity manifest has stale expected canonical IDs"
        );
        move_profiles(connection, reviewed, &rows, &ids).await?;
    }
    let result_count =
        sqlx::query_scalar("SELECT count(*) FROM results WHERE athlete_source_id=ANY($1)")
            .bind(ids)
            .fetch_one(connection)
            .await?;
    Ok(Report {
        canonical_athlete_id: reviewed.manifest.canonical_athlete_id,
        profile_count: rows.len(),
        result_count,
        already_applied,
        manifest_sha256: reviewed.sha256.clone(),
    })
}

async fn move_profiles(
    connection: &mut PgConnection,
    reviewed: &Reviewed,
    rows: &[Existing],
    ids: &[i64],
) -> Result<()> {
    let manifest = &reviewed.manifest;
    let keep = manifest.canonical_athlete_id;
    let previous = rows
        .iter()
        .map(|row| row.athlete_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let warnings = fact_warnings(rows);
    let audit = json!({
        "schema_revision": 1, "canonical_athlete_id": keep, "preferred_name": manifest.preferred_name,
        "accepted_names": manifest.accepted_names.iter().map(|name| normalized(name)).collect::<Vec<_>>(),
        "accepted_published_names": manifest.accepted_names, "reviewed_by": manifest.reviewed_by,
        "reason": manifest.reason, "manifest_sha256": reviewed.sha256,
        "profiles": manifest.profiles, "original_profiles": rows,
        "fact_warnings": warnings, "latest_fact_conflicts": {},
        "previous_canonical_ids": previous, "retired_canonical_ids": previous.iter().copied().filter(|id| *id != keep).collect::<Vec<_>>()
    });
    sqlx::query("WITH previous AS (SELECT max(state) state,max(gender) gender,max(grade) grade,max(graduation_year) graduation_year,min(school_id) school_id,bool_or(needs_review) needs_review FROM athletes WHERE id=ANY($3)) UPDATE athletes a SET name=$2,state=COALESCE(a.state,p.state),gender=COALESCE(a.gender,p.gender),grade=COALESCE(a.grade,p.grade),graduation_year=COALESCE(a.graduation_year,p.graduation_year),school_id=COALESCE(a.school_id,p.school_id),needs_review=a.needs_review OR p.needs_review OR $4 FROM previous p WHERE a.id=$1")
        .bind(keep).bind(&manifest.preferred_name).bind(&previous).bind(!warnings.is_empty()).execute(&mut *connection).await?;
    sqlx::query("UPDATE results SET athlete_id=$1 WHERE athlete_source_id=ANY($2)")
        .bind(keep)
        .bind(ids)
        .execute(&mut *connection)
        .await?;
    sqlx::query("UPDATE athlete_sources SET athlete_id=$1,details=jsonb_set(details,'{_reviewed_identity}',$3::jsonb || jsonb_build_object('recorded_at',clock_timestamp())) WHERE id=ANY($2)")
        .bind(keep).bind(ids).bind(audit).execute(&mut *connection).await?;
    sqlx::query("DELETE FROM athletes WHERE id=ANY($1) AND id<>$2")
        .bind(&previous)
        .bind(keep)
        .execute(connection)
        .await?;
    Ok(())
}
