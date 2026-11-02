use std::{sync::Arc, time::Instant};

use anyhow::Result;
use sqlx::PgConnection;

use super::{
    Cache, auxiliary, delta, load, membership,
    model::{Key, Prepared, Snapshot},
    update,
};

pub(super) async fn run(
    connection: &mut PgConnection,
    cache: &Cache,
    key: &Key,
    previous: Option<&Arc<Snapshot>>,
) -> Result<Arc<Prepared>> {
    let started = Instant::now();
    let change_set = if let Some(previous) = previous {
        let changes = delta::changes(connection, &previous.visibility).await?;
        if changes.is_empty() {
            return Ok(Arc::clone(&previous.prepared));
        }
        delta::expand(connection, cache, changes, Arc::clone(&previous.prepared)).await?
    } else {
        delta::Delta {
            ids: None,
            auxiliary: true,
        }
    };
    let delta_ms = started.elapsed().as_secs_f64() * 1000.0;
    let loading = Instant::now();
    let ids = change_set.ids.as_deref();
    let previous_data = previous
        .filter(|_| ids.is_some())
        .map(|snapshot| &snapshot.prepared);
    let facts = load::facts(connection, key, ids).await?;
    let operations = load::operations(connection, key, ids).await?;
    let load_ms = loading.elapsed().as_secs_f64() * 1000.0;
    let auxiliary_started = Instant::now();
    let (facts, auxiliary) = auxiliary::refresh(
        connection,
        cache,
        previous_data.cloned(),
        facts,
        change_set.auxiliary,
    )
    .await?;
    let auxiliary_ms = auxiliary_started.elapsed().as_secs_f64() * 1000.0;
    tracing::debug!(
        delta_ms,
        load_ms,
        auxiliary_ms,
        changed_pages = ids.map(<[i64]>::len),
        "coverage refresh stages"
    );
    let key = key.clone();
    let previous_data = previous_data.cloned();
    let changed_ids = ids.map(<[i64]>::to_vec);
    cache
        .compiler
        .run(move || {
            let merging = Instant::now();
            let facts = load::merge_facts(
                previous_data.as_ref().map(|previous| &previous.facts),
                changed_ids.as_deref(),
                facts,
            )?;
            let operations = load::merge_operations(
                previous_data.as_ref().map(|previous| &previous.operations),
                changed_ids.as_deref(),
                operations,
            )?;
            if let Some(previous) = previous_data.as_ref().filter(|previous| {
                Arc::ptr_eq(&previous.facts, &facts)
                    && Arc::ptr_eq(&previous.operations, &operations)
                    && previous.auxiliary.as_ref() == auxiliary.as_ref()
            }) {
                return Ok(Arc::clone(previous));
            }
            let merge_ms = merging.elapsed().as_secs_f64() * 1000.0;
            let compiling = Instant::now();
            let membership = membership::Membership::updated(
                previous_data
                    .as_ref()
                    .map(|previous| (&previous.membership, previous.facts.as_ref())),
                &facts,
                changed_ids.as_deref(),
            );
            let candidates = update::candidates(
                &key,
                &facts,
                &operations,
                &membership,
                &auxiliary,
                previous_data.as_deref(),
                changed_ids.as_deref(),
            )?;
            tracing::debug!(
                merge_ms,
                compile_ms = compiling.elapsed().as_secs_f64() * 1000.0,
                facts = facts.len(),
                operations = operations.len(),
                "coverage compiler stages"
            );
            Ok(Arc::new(Prepared {
                facts,
                operations,
                auxiliary,
                candidates,
                membership,
            }))
        })
        .await
}
