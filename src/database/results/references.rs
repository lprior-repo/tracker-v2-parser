use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use sqlx::PgConnection;

use crate::model::{Performance, Source};

use super::super::{identity, schools};

pub(super) const ROWS: usize = 128;
const BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(super) struct References {
    profiles: BTreeMap<String, i64>,
    schools: BTreeMap<String, i64>,
}

impl References {
    #[tracing::instrument(skip_all)]
    pub(super) async fn load(
        connection: &mut PgConnection,
        source: Source,
        rows: &[Performance],
    ) -> Result<Self> {
        let bytes = rows.iter().try_fold(0_usize, |total, row| {
            total
                .checked_add(row.athlete_id.len())
                .and_then(|size| size.checked_add(row.school_id.as_ref().map_or(0, String::len)))
                .context("result reference byte count overflow")
        })?;
        if bytes > BYTES {
            return Ok(Self::default());
        }
        let profiles = rows
            .iter()
            .map(|row| row.athlete_id.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let schools = rows
            .iter()
            .filter_map(|row| row.school_id.as_deref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let profiles = sqlx::query_as::<_, (String, i64)>(
            "SELECT source_id,id FROM athlete_sources WHERE source=$1 AND source_id=ANY($2) AND name IS NOT NULL",
        ).bind(source.as_str()).bind(profiles).fetch_all(&mut *connection).await?
            .into_iter().collect();
        let schools = if schools.is_empty() {
            BTreeMap::new()
        } else {
            sqlx::query_as::<_, (String, i64)>(
                "SELECT source_id,id FROM schools WHERE source=$1 AND source_id=ANY($2)",
            )
            .bind(source.as_str())
            .bind(schools)
            .fetch_all(connection)
            .await?
            .into_iter()
            .collect()
        };
        Ok(Self { profiles, schools })
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn profile(
        &self,
        connection: &mut PgConnection,
        source: Source,
        id: &str,
    ) -> Result<i64> {
        match self.profiles.get(id) {
            Some(profile) => Ok(*profile),
            None => identity::find(connection, source, id).await,
        }
    }

    #[tracing::instrument(skip_all)]
    pub(super) async fn school(
        &self,
        connection: &mut PgConnection,
        source: Source,
        id: Option<&str>,
    ) -> Result<Option<i64>> {
        match id.and_then(|id| self.schools.get(id)) {
            Some(school) => Ok(Some(*school)),
            None => schools::find(connection, source, id).await,
        }
    }
}
