use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use futures::{TryStreamExt, stream};
use serde_json::Value;
use sqlx::PgConnection;

use crate::model::{PageContext, PageKind, Performance, Source, Target};

use super::{keys, school_evidence};

mod masked;
pub(super) use masked::review as review_masked;

type Incoming<'a> = BTreeMap<(&'a str, &'a str), (usize, usize)>;
#[derive(Default)]
struct Matches {
    rows: BTreeMap<usize, Vec<i64>>,
    unvalidated: Vec<i64>,
}

#[tracing::instrument(skip(connection, performances))]
pub(super) async fn reconcile(
    connection: &mut PgConnection,
    source: Source,
    performances: &[Performance],
    location: &str,
) -> Result<()> {
    if source != Source::MileSplit {
        return Ok(());
    }
    let incoming = incoming(performances);
    if incoming.is_empty() {
        return Ok(());
    }
    let matches = retained_matches(connection, &incoming).await?;
    if !matches.unvalidated.is_empty() {
        flag_ambiguity(connection, &matches.unvalidated).await?;
    }
    let incoming = &incoming;
    stream::iter(matches.rows.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, (index, ids)| async move {
            let Some(performance) = performances.get(index) else { return Ok(connection); };
            let unique = performance.result_id.as_deref().and_then(|id| incoming.get(&(performance.athlete_id.as_str(), id)))
                .is_some_and(|(_, count)| *count == 1);
            if !unique || ids.len() != 1 {
                flag_ambiguity(connection, &ids).await?;
                return Ok(connection);
            }
            let Some(id) = ids.first() else { return Ok(connection); };
            let key = keys::performance(source, performance, location, index)?;
            let updated = sqlx::query(
                "UPDATE results SET source_result_key=$1,source_result_id=$2 WHERE id=$3 \
                 AND NOT EXISTS (SELECT 1 FROM results current_result WHERE current_result.source_result_key=$1 AND current_result.id<>$3)",
            ).bind(key).bind(&performance.result_id).bind(id).execute(&mut *connection).await?;
            if updated.rows_affected() != 1 {
                flag_ambiguity(connection, &ids).await?;
            }
            Ok(connection)
        }).await.map(|_| ())
}

fn incoming(performances: &[Performance]) -> Incoming<'_> {
    performances
        .iter()
        .enumerate()
        .filter_map(|(index, performance)| {
            let values = performance.source_values.as_ref()?;
            let basis = values.get("identity_basis")?.as_str()?;
            if values.get("kind")?.as_str()? != "career_result"
                || !matches!(basis, "published_result" | "unique_context")
            {
                return None;
            }
            Some((
                (
                    performance.athlete_id.as_str(),
                    performance.result_id.as_deref()?,
                ),
                index,
            ))
        })
        .fold(BTreeMap::new(), |mut incoming, (key, index)| {
            let entry = incoming.entry(key).or_insert((index, 0_usize));
            entry.1 = entry.1.saturating_add(1);
            incoming
        })
}

#[tracing::instrument(skip(connection, incoming))]
async fn retained_matches(
    connection: &mut PgConnection,
    incoming: &Incoming<'_>,
) -> Result<Matches> {
    let athletes = incoming
        .keys()
        .map(|(athlete, _)| *athlete)
        .collect::<std::collections::BTreeSet<_>>();
    let athletes = athletes.into_iter().collect::<Vec<_>>();
    sqlx::query_as::<_, (i64, String, String, Value)>(
        "SELECT r.id,p.source_id,r.source_url,r.raw FROM results r \
         JOIN athlete_sources p ON p.id=r.athlete_source_id AND p.source='milesplit' \
         WHERE r.source='milesplit' AND p.source_id=ANY($1) \
         AND r.source_result_id ~ '^profile-history:[0-9]+:[[:xdigit:]]{64}$' \
         AND r.raw #>> '{source_values,kind}'='career_result' \
         ORDER BY r.id FOR UPDATE OF r",
    )
    .bind(athletes)
    .fetch(connection)
    .map_err(anyhow::Error::from)
    .try_fold(
        Matches::default(),
        |mut matches, (id, athlete, location, raw)| async move {
            if raw.get("athlete_id").and_then(Value::as_str) != Some(athlete.as_str())
                || school_evidence::profile_url(&location, &raw).is_none()
            {
                matches.unvalidated.push(id);
                return Ok(matches);
            }
            let Some(html) = raw
                .pointer("/source_values/row/html")
                .and_then(Value::as_str)
            else {
                matches.unvalidated.push(id);
                return Ok(matches);
            };
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                location,
                PageContext::default(),
            );
            let identity = match crate::sources::milesplit::profile_history_identity(
                &target, &athlete, html,
            ) {
                Ok(Some((identity, _))) => Some(identity),
                Ok(None) => None,
                Err(_) => {
                    tracing::warn!(
                        result_id = id,
                        "preserving retained history with invalid published context"
                    );
                    None
                }
            };
            let Some(identity) = identity else {
                matches.unvalidated.push(id);
                return Ok(matches);
            };
            if let Some((index, _)) = incoming.get(&(athlete.as_str(), identity.as_str())) {
                matches.rows.entry(*index).or_insert_with(Vec::new).push(id);
            }
            Ok(matches)
        },
    )
    .await
}

#[tracing::instrument(skip(connection, ids))]
async fn flag_ambiguity(connection: &mut PgConnection, ids: &[i64]) -> Result<()> {
    sqlx::query(
        "UPDATE results SET raw=jsonb_set(jsonb_set(raw,'{source_values,identity_reconciliation}', \
         '\"ambiguous_legacy_context\"'::jsonb,TRUE),'{source_values,revision_status}', \
         '\"unverified_prior_revision\"'::jsonb,TRUE) WHERE id=ANY($1)",
    )
    .bind(ids)
    .execute(&mut *connection)
    .await?;
    sqlx::query("UPDATE athletes SET needs_review=TRUE WHERE id IN (SELECT athlete_id FROM results WHERE id=ANY($1))")
        .bind(ids).execute(connection).await?;
    Ok(())
}

pub(super) async fn review(
    connection: &mut PgConnection,
    performances: &[Performance],
) -> Result<()> {
    masked::reconfirm(connection, performances).await?;
    let contexts = performances.iter().try_fold(
        BTreeMap::<String, Vec<&str>>::new(),
        |mut contexts, row| -> Result<_> {
            if let Some(values) = &row.source_values
                && let Some(id) = row.result_id.as_deref()
                && let Some(context) = context(&row.athlete_id, values)?
            {
                contexts.entry(context).or_default().push(id);
            }
            Ok(contexts)
        },
    )?;
    if contexts.is_empty() {
        return Ok(());
    }
    let athletes = performances
        .iter()
        .map(|row| row.athlete_id.as_str())
        .collect::<Vec<_>>();
    let rows = sqlx::query_as::<_, (i64, String, String, Value)>(
        "SELECT r.id,p.source_id,r.source_result_id,r.raw->'source_values' FROM results r \
         JOIN athlete_sources p ON p.id=r.athlete_source_id \
         WHERE r.source='milesplit' AND p.source='milesplit' AND p.source_id=ANY($1) \
         AND r.source_result_id IS NOT NULL AND r.raw#>>'{source_values,kind}'='career_result' ORDER BY r.id LIMIT 10001 FOR UPDATE OF r",
    )
    .bind(athletes)
    .fetch_all(&mut *connection)
    .await?;
    ensure!(
        rows.len() <= 10000,
        "career replay review exceeds 10000 observations"
    );
    stream::iter(rows.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, (id, athlete, identity, values)| {
            let contexts = &contexts;
            async move {
                if let Some(context) = context(&athlete, &values)?
                    && let Some(observed) = contexts.get(&context)
                {
                    let confirmed = observed.contains(&identity.as_str());
                    set_revision(connection, id, confirmed).await?;
                }
                Ok(connection)
            }
        })
        .await
        .map(|_| ())
}

fn context(athlete: &str, values: &Value) -> Result<Option<String>> {
    if values.get("kind").and_then(Value::as_str) != Some("career_result") {
        return Ok(None);
    }
    let Some(html) = values.pointer("/row/html").and_then(Value::as_str) else {
        return Ok(None);
    };
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://www.milesplit.com/athletes/{athlete}"),
        PageContext::default(),
    );
    Ok(
        crate::sources::milesplit::profile_history_identity(&target, athlete, html)?
            .map(|(identity, _)| identity),
    )
}

async fn set_revision(connection: &mut PgConnection, id: i64, confirmed: bool) -> Result<()> {
    if confirmed {
        sqlx::query("UPDATE results SET raw=raw #- '{source_values,revision_status}' WHERE id=$1 AND raw#>>'{source_values,revision_status}'='unverified_prior_revision'")
            .bind(id).execute(connection).await?;
    } else {
        sqlx::query("UPDATE results SET raw=jsonb_set(raw,'{source_values,revision_status}','\"unverified_prior_revision\"'::jsonb,TRUE) WHERE id=$1")
            .bind(id).execute(&mut *connection).await?;
        sqlx::query("UPDATE athletes SET needs_review=TRUE WHERE id=(SELECT athlete_id FROM results WHERE id=$1)")
            .bind(id).execute(connection).await?;
    }
    Ok(())
}
