use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};

use crate::marks;

use super::manifest::{Prepared, ResultLink};

#[derive(Debug, Serialize)]
pub struct Report {
    links_added: u64,
    links_already_present: u64,
    manifest_sha256: String,
}

#[derive(Debug, FromRow)]
struct ResultRow {
    id: i64,
    source_result_key: String,
    source_result_id: Option<String>,
    source: String,
    athlete_source_id: Option<i64>,
    athlete_id: Option<i64>,
    represented_school_id: Option<i64>,
    event: String,
    raw_mark: String,
    sport: String,
    meet_id: Option<String>,
    meet_name: Option<String>,
    meet_date: Option<String>,
    wind: Option<f64>,
    timing: Option<String>,
    specification: Option<String>,
    course: Option<String>,
    status: String,
    is_personal_record: bool,
    is_relay: bool,
    season: Option<i32>,
    grade: Option<String>,
    gender: Option<String>,
    source_url: String,
    raw: Value,
    normalized_numeric: Option<i64>,
    normalized_unit: Option<String>,
}

#[derive(Debug, FromRow)]
struct TargetRow {
    athlete_source_id: i64,
    source: String,
    source_id: String,
    profile_url: Option<String>,
    athlete_id: i64,
    profile_name: Option<String>,
    profile_school_id: Option<i64>,
    profile_gender: Option<String>,
    profile_grade: Option<String>,
    profile_graduation_year: Option<i32>,
    athlete_name: Option<String>,
    athlete_gender: Option<String>,
    athlete_graduation_year: Option<i32>,
    athlete_school_id: Option<i64>,
    athlete_state: Option<String>,
    school_source: Option<String>,
    school_source_id: Option<String>,
    school_name: Option<String>,
    school_state: Option<String>,
}

struct PreparedLink {
    result: ResultRow,
    target: TargetRow,
    evidence: Value,
}

pub(super) async fn apply(connection: &mut PgConnection, prepared: Prepared) -> Result<Report> {
    let evidence = prepared.evidence();
    let reviewed_by = prepared.reviewed_by();
    let researcher = prepared.researcher();
    let reviewed_at = prepared.reviewed_at();
    let authorization_quote = prepared.authorization_quote();
    let manifest_sha256 = prepared.sha256();
    let (_, added, present) = stream::iter(prepared.links().iter().map(Ok::<_, anyhow::Error>))
        .try_fold(
            (connection, 0_u64, 0_u64),
            |(connection, added, present), link| async move {
                let result = lock_result(connection, link).await?;
                let target = lock_target(connection, link).await?;
                validate_match(link, &result, &target)?;
                let evidence = link.evidence_values(evidence)?;
                let claim = PreparedLink {
                    result,
                    target,
                    evidence,
                };
                let inserted = insert_or_reuse(
                    connection,
                    &claim,
                    reviewed_by,
                    researcher,
                    reviewed_at,
                    authorization_quote,
                    manifest_sha256,
                )
                .await?;
                let added = added
                    .checked_add(u64::from(inserted))
                    .context("reviewed result-link added count overflow")?;
                let present = present
                    .checked_add(u64::from(!inserted))
                    .context("reviewed result-link present count overflow")?;
                Ok((connection, added, present))
            },
        )
        .await?;
    Ok(Report {
        links_added: added,
        links_already_present: present,
        manifest_sha256: prepared.sha256().to_owned(),
    })
}

async fn lock_result(connection: &mut PgConnection, link: &ResultLink) -> Result<ResultRow> {
    sqlx::query_as::<_, ResultRow>(
        "SELECT id,source_result_key,source_result_id,source,athlete_source_id,athlete_id, \
         represented_school_id,event,raw_mark,sport,meet_id,meet_name,meet_date,wind,timing, \
         specification,course,status,is_personal_record,is_relay,season,grade,gender,source_url, \
         raw,normalized_numeric,normalized_unit FROM results WHERE source_result_key=$1 FOR UPDATE",
    )
    .bind(link.key())
    .fetch_optional(connection)
    .await?
    .context("reviewed result key does not exist")
}

async fn lock_target(connection: &mut PgConnection, link: &ResultLink) -> Result<TargetRow> {
    sqlx::query_as::<_, TargetRow>(
        "SELECT p.id AS athlete_source_id,p.source,p.source_id,p.profile_url,p.athlete_id, \
         p.name AS profile_name,p.school_id AS profile_school_id,p.gender AS profile_gender, \
         p.grade AS profile_grade,p.graduation_year AS profile_graduation_year, \
         a.name AS athlete_name,a.gender AS athlete_gender, \
         a.graduation_year AS athlete_graduation_year,a.school_id AS athlete_school_id, \
         a.state AS athlete_state,s.source AS school_source,s.source_id AS school_source_id, \
         s.name AS school_name,s.state AS school_state FROM athlete_sources p \
         JOIN athletes a ON a.id=p.athlete_id JOIN schools s ON s.id=p.school_id \
         WHERE p.id=$1 FOR UPDATE OF p,a,s",
    )
    .bind(link.target_athlete_source_id)
    .fetch_optional(connection)
    .await?
    .context("reviewed target profile or its school no longer exists")
}

fn validate_match(link: &ResultLink, result: &ResultRow, target: &TargetRow) -> Result<()> {
    let expected_mark = link.mark.as_str();
    ensure!(
        result.source_result_key == link.source_result_key
            && result.source_result_id.as_deref() == Some(link.source_result_id.as_str())
            && result.source == link.source.as_str()
            && result.athlete_source_id.is_none()
            && result.athlete_id.is_none(),
        "reviewed result is missing, changed, or no longer unassigned"
    );
    ensure!(
        result.source_url == link.source_url
            && result.event == link.event
            && result.raw_mark.as_str() == expected_mark
            && result.sport == link.sport.as_str()
            && result.meet_id.as_deref() == Some(link.meet_id.as_str())
            && result.meet_date.as_deref() == Some(link.meet_date.as_str())
            && result.status == link.status
            && result.represented_school_id == link.represented_school_id,
        "reviewed result facts no longer match the research manifest"
    );
    validate_raw_row(link, &result.raw)?;
    validate_mark(result)?;
    validate_gender_compatibility(
        result.gender.as_deref(),
        &result.event,
        target.profile_gender.as_deref(),
        target.athlete_gender.as_deref(),
    )?;
    validate_target(link, target)
}

fn validate_gender_compatibility(
    result_gender: Option<&str>,
    event: &str,
    profile_gender: Option<&str>,
    canonical_gender: Option<&str>,
) -> Result<()> {
    let result_gender = known_gender(result_gender);
    let event_gender = event_gender(event);
    let profile_gender = known_gender(profile_gender);
    let canonical_gender = known_gender(canonical_gender);
    let known = [
        result_gender,
        event_gender,
        profile_gender,
        canonical_gender,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    ensure!(
        known.windows(2).all(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_some_and(|(left, right)| left == right)
        }),
        "reviewed result, profile, and canonical athlete genders conflict"
    );
    Ok(())
}

fn known_gender(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "f" | "female" | "girl" | "girls" => Some(false),
        "m" | "male" | "boy" | "boys" => Some(true),
        _ => None,
    }
}

fn event_gender(event: &str) -> Option<bool> {
    let (label, _) = event.trim().split_once(char::is_whitespace)?;
    known_gender(Some(label))
}

fn validate_raw_row(link: &ResultLink, raw: &Value) -> Result<()> {
    let row = raw.get("raw").context("reviewed result lost its raw row")?;
    ensure!(
        row.get("kind").and_then(Value::as_str) == Some("raw_result")
            && row.get("name").and_then(Value::as_str) == Some(link.raw_name.as_str())
            && row.get("team").and_then(Value::as_str) == Some(link.raw_team.as_str())
            && row.get("grade").and_then(Value::as_str) == Some(link.raw_grade.as_str())
            && row.get("published_file_identity").and_then(Value::as_str)
                == Some(link.published_file_identity.as_str())
            && row.get("line_ordinal").and_then(Value::as_i64) == Some(link.line_ordinal)
            && row.get("published_html").and_then(Value::as_str)
                == Some(link.published_html.as_str())
            && raw.get("result_id").and_then(Value::as_str) == Some(link.source_result_id.as_str())
            && raw.get("event").and_then(Value::as_str) == Some(link.event.as_str())
            && raw.get("mark").and_then(Value::as_str) == Some(link.mark.as_str())
            && raw.get("date").and_then(Value::as_str) == Some(link.meet_date.as_str()),
        "reviewed result raw row no longer matches the published evidence"
    );
    Ok(())
}

fn validate_mark(result: &ResultRow) -> Result<()> {
    ensure!(
        !result.is_relay && result.status == "recorded",
        "reviewed result link requires a recorded individual result"
    );
    let (normalized, measure, _) = marks::normalized_mark(&result.event, &result.raw_mark)
        .context("reviewed result mark is not readable for its event")?;
    ensure!(
        result.normalized_numeric == Some(normalized)
            && result.normalized_unit.as_deref() == Some(measure.unit()),
        "reviewed result normalization is missing or inconsistent"
    );
    Ok(())
}

fn validate_target(link: &ResultLink, target: &TargetRow) -> Result<()> {
    ensure!(
        target.athlete_source_id == link.target_athlete_source_id
            && target.athlete_id == link.target_athlete_id
            && target.source == link.source.as_str()
            && target.source_id == link.target_source_id
            && target.profile_url.as_deref() == Some(link.target_profile_url.as_str()),
        "reviewed target source identity or profile URL is stale or conflicting"
    );
    ensure!(
        target.profile_name.as_deref() == Some(link.target_name.as_str())
            && target.profile_gender == link.target_gender
            && target.profile_graduation_year == Some(link.target_graduation_year)
            && target.profile_school_id == Some(link.target_school_id),
        "reviewed target profile name, gender, graduation year, or school is stale or conflicting"
    );
    ensure!(
        target.school_name.as_deref() == Some(link.target_school_name.as_str())
            && target.school_state.as_deref() == Some(link.target_school_state.as_str()),
        "reviewed target school facts are stale or conflicting"
    );
    ensure!(
        target.athlete_name.as_deref() == Some(link.target_name.as_str())
            && target.athlete_school_id == Some(link.target_school_id)
            && target.athlete_graduation_year == Some(link.target_graduation_year),
        "reviewed canonical athlete identity facts are stale or conflicting"
    );
    ensure!(
        target.school_source.is_some()
            && target
                .school_source_id
                .as_deref()
                .is_some_and(|id| !id.is_empty()),
        "reviewed target school lacks a source identity"
    );
    Ok(())
}

async fn insert_or_reuse(
    connection: &mut PgConnection,
    claim: &PreparedLink,
    reviewed_by: &str,
    researcher: &str,
    reviewed_at: &str,
    authorization_quote: &str,
    manifest_sha256: &str,
) -> Result<bool> {
    let result_snapshot = result_snapshot(&claim.result);
    let target_snapshot = target_snapshot(&claim.target);
    let inserted = sqlx::query(
        "INSERT INTO reviewed_result_links(source_result_key,target_athlete_source_id,reviewed_by, \
         researcher,reviewed_at,authorization_quote,manifest_sha256,evidence,result_snapshot,target_snapshot) \
         VALUES($1,$2,$3,$4,$5::timestamptz,$6,$7,$8,$9,$10) ON CONFLICT(source_result_key) DO NOTHING",
    )
    .bind(&claim.result.source_result_key)
    .bind(claim.target.athlete_source_id)
    .bind(reviewed_by)
    .bind(researcher)
    .bind(reviewed_at)
    .bind(authorization_quote)
    .bind(manifest_sha256)
    .bind(&claim.evidence)
    .bind(&result_snapshot)
    .bind(&target_snapshot)
    .execute(&mut *connection)
    .await?;
    if inserted.rows_affected() == 1 {
        return Ok(true);
    }
    let existing: (i64, String, Value, Value) = sqlx::query_as(
        "SELECT target_athlete_source_id,manifest_sha256,result_snapshot,target_snapshot \
         FROM reviewed_result_links WHERE source_result_key=$1 FOR UPDATE",
    )
    .bind(&claim.result.source_result_key)
    .fetch_one(connection)
    .await?;
    ensure!(
        existing.0 == claim.target.athlete_source_id
            && existing.1 == manifest_sha256
            && existing.2 == result_snapshot
            && existing.3 == target_snapshot,
        "result already has a conflicting reviewed link"
    );
    Ok(false)
}

fn result_snapshot(row: &ResultRow) -> Value {
    json!({
        "result_id":row.id,
        "source_result_key":row.source_result_key,
        "source_result_id":row.source_result_id,
        "source":row.source,
        "athlete_source_id":row.athlete_source_id,
        "athlete_id":row.athlete_id,
        "represented_school_id":row.represented_school_id,
        "event":row.event,
        "raw_mark":row.raw_mark,
        "sport":row.sport,
        "meet_id":row.meet_id,
        "meet_name":row.meet_name,
        "meet_date":row.meet_date,
        "wind":row.wind,
        "timing":row.timing,
        "specification":row.specification,
        "course":row.course,
        "status":row.status,
        "is_personal_record":row.is_personal_record,
        "is_relay":row.is_relay,
        "season":row.season,
        "grade":row.grade,
        "gender":row.gender,
        "source_url":row.source_url,
        "raw":row.raw,
        "normalized_numeric":row.normalized_numeric,
        "normalized_unit":row.normalized_unit
    })
}

fn target_snapshot(row: &TargetRow) -> Value {
    json!({
        "athlete_source_id":row.athlete_source_id,
        "source":row.source,
        "source_id":row.source_id,
        "profile_url":row.profile_url,
        "athlete_id":row.athlete_id,
        "profile_name":row.profile_name,
        "profile_school_id":row.profile_school_id,
        "profile_gender":row.profile_gender,
        "profile_grade":row.profile_grade,
        "profile_graduation_year":row.profile_graduation_year,
        "athlete_name":row.athlete_name,
        "athlete_gender":row.athlete_gender,
        "athlete_graduation_year":row.athlete_graduation_year,
        "athlete_school_id":row.athlete_school_id,
        "athlete_state":row.athlete_state,
        "school_source":row.school_source,
        "school_source_id":row.school_source_id,
        "school_name":row.school_name,
        "school_state":row.school_state
    })
}
