use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Result, bail};
use sqlx::PgConnection;

use super::{Cache, model::Prepared};

#[derive(sqlx::FromRow)]
pub(super) struct Change {
    kind: String,
    identity: String,
    auxiliary: bool,
}

#[derive(Default)]
struct Impact {
    pages: BTreeSet<i64>,
    profiles: BTreeSet<i64>,
    athletes: BTreeSet<i64>,
    schools: BTreeSet<i64>,
    operations: BTreeSet<String>,
    auxiliary: bool,
    reset: bool,
}

impl Impact {
    fn include(&mut self, change: Change) -> Result<()> {
        self.auxiliary |= change.auxiliary;
        match change.kind.as_str() {
            "page" => {
                self.pages.insert(change.identity.parse()?);
            }
            "profile" => {
                self.profiles.insert(change.identity.parse()?);
            }
            "athlete" => {
                self.athletes.insert(change.identity.parse()?);
            }
            "school" => {
                self.schools.insert(change.identity.parse()?);
            }
            "operation" => {
                self.operations.insert(change.identity);
            }
            "contact" | "school_contact" => self.auxiliary = true,
            "reset" => self.reset = true,
            kind => bail!("unknown selector change entity {kind}"),
        }
        Ok(())
    }

    fn from_changes(changes: Vec<Change>, previous: &Prepared) -> Result<Self> {
        let mut impact = changes
            .into_iter()
            .try_fold(Self::default(), |mut impact, change| {
                impact.include(change)?;
                Ok::<_, anyhow::Error>(impact)
            })?;
        if !impact.profiles.is_empty() || !impact.athletes.is_empty() || !impact.schools.is_empty()
        {
            impact.pages.extend(
                previous
                    .facts
                    .values()
                    .filter(|fact| {
                        fact.profile_id
                            .is_some_and(|id| impact.profiles.contains(&id))
                            || fact
                                .athlete_id
                                .is_some_and(|id| impact.athletes.contains(&id))
                            || fact
                                .school_id
                                .is_some_and(|id| impact.schools.contains(&id))
                    })
                    .map(|fact| fact.id),
            );
        }
        if !impact.operations.is_empty() {
            impact.pages.extend(
                previous
                    .operations
                    .values()
                    .filter(|operation| impact.operations.contains(&operation.identity))
                    .map(|operation| operation.page_id),
            );
        }
        Ok(impact)
    }
}

pub(super) struct Delta {
    pub ids: Option<Vec<i64>>,
    pub auxiliary: bool,
}

#[tracing::instrument(skip_all)]
pub(super) async fn changes(
    connection: &mut PgConnection,
    visibility: &str,
) -> Result<Vec<Change>> {
    Ok(sqlx::query_as("SELECT kind,identity,auxiliary FROM selector_changes WHERE changed_xid>=pg_snapshot_xmax($1::pg_snapshot) AND changed_xid<pg_snapshot_xmax(pg_current_snapshot()) UNION ALL SELECT c.kind,c.identity,c.auxiliary FROM pg_snapshot_xip($1::pg_snapshot) active(xid) JOIN selector_changes c ON c.changed_xid=active.xid LIMIT 1025")
        .bind(visibility).fetch_all(connection).await?)
}

#[tracing::instrument(skip_all)]
pub(super) async fn expand(
    connection: &mut PgConnection,
    cache: &Cache,
    changes: Vec<Change>,
    previous: Arc<Prepared>,
) -> Result<Delta> {
    if changes.len() > 1024 {
        return Ok(Delta {
            ids: None,
            auxiliary: true,
        });
    }
    let mut impact = cache
        .compiler
        .run(move || Impact::from_changes(changes, &previous))
        .await?;
    if impact.reset {
        return Ok(Delta {
            ids: None,
            auxiliary: true,
        });
    }
    if impact.profiles.is_empty()
        && impact.athletes.is_empty()
        && impact.schools.is_empty()
        && impact.operations.is_empty()
    {
        return Ok(Delta {
            ids: Some(impact.pages.into_iter().collect()),
            auxiliary: impact.auxiliary,
        });
    }
    let ids: Vec<i64>=sqlx::query_scalar("WITH changed_profiles AS MATERIALIZED (SELECT source,source_id FROM athlete_sources WHERE id=ANY($1::bigint[]) UNION SELECT source,source_id FROM athlete_sources WHERE athlete_id=ANY($2::bigint[]) UNION SELECT source,source_id FROM athlete_sources WHERE school_id=ANY($3::bigint[])) SELECT p.id FROM changed_profiles profile JOIN scrape_pages p ON p.source=profile.source AND p.cursor->'context'->>'athlete_id'=profile.source_id WHERE p.cursor->>'kind'='athlete' AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?(/(stats|progression))?/?$' UNION SELECT page_id FROM durable_page_operations WHERE operation_id=ANY($4::text[]) LIMIT 8193")
        .bind(impact.profiles.into_iter().collect::<Vec<_>>()).bind(impact.athletes.into_iter().collect::<Vec<_>>())
        .bind(impact.schools.into_iter().collect::<Vec<_>>()).bind(impact.operations.into_iter().collect::<Vec<_>>())
        .fetch_all(connection).await?;
    impact.pages.extend(ids);
    if impact.pages.len() > 8192 {
        return Ok(Delta {
            ids: None,
            auxiliary: true,
        });
    }
    Ok(Delta {
        ids: Some(impact.pages.into_iter().collect()),
        auxiliary: impact.auxiliary,
    })
}
