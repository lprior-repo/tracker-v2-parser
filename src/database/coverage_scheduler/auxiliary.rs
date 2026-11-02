use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, Result};
use sqlx::PgConnection;

use super::{
    Cache, load,
    model::{Auxiliary, Fact, Prepared},
};

struct Scoped {
    facts: Vec<Fact>,
    official_urls: Vec<String>,
    authorities: HashSet<String>,
}

fn scope(previous: Option<&Prepared>, facts: Vec<Fact>) -> Scoped {
    let mut official_urls: Vec<String> = previous
        .into_iter()
        .flat_map(|prepared| prepared.facts.values())
        .map(AsRef::as_ref)
        .chain(facts.iter())
        .filter(|fact| fact.official && fact.stage == 3)
        .map(|fact| fact.url.clone())
        .collect();
    let authorities = previous
        .into_iter()
        .flat_map(|prepared| prepared.facts.values())
        .map(AsRef::as_ref)
        .chain(facts.iter())
        .filter(|fact| fact.stage == 3)
        .filter_map(|fact| fact.authority.clone())
        .collect();
    official_urls.sort_unstable();
    official_urls.dedup();
    Scoped {
        facts,
        official_urls,
        authorities,
    }
}

pub(super) async fn refresh(
    connection: &mut PgConnection,
    cache: &Cache,
    previous: Option<Arc<Prepared>>,
    facts: Vec<Fact>,
    changed: bool,
) -> Result<(Vec<Fact>, Arc<Auxiliary>)> {
    if !changed {
        return Ok((
            facts,
            Arc::clone(
                &previous
                    .context("missing coverage auxiliary state")?
                    .auxiliary,
            ),
        ));
    }
    let scoped = cache
        .compiler
        .run(move || Ok(scope(previous.as_deref(), facts)))
        .await?;
    let auxiliary = load::auxiliary(connection, &scoped.official_urls, &scoped.authorities).await?;
    Ok((scoped.facts, Arc::new(auxiliary)))
}
