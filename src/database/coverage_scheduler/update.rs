use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};

use super::{
    compile,
    index::Index,
    membership::Membership,
    model::{Auxiliary, Candidate, Fact, Key, Operation, Prepared},
};

fn affected(
    previous: &Prepared,
    facts: &Index<Arc<Fact>>,
    membership: &Membership,
    auxiliary: &Auxiliary,
    ids: &[i64],
) -> BTreeSet<String> {
    let changed: Vec<&Arc<Fact>> = ids
        .iter()
        .flat_map(|id| previous.facts.get(*id).into_iter().chain(facts.get(*id)))
        .collect();
    let mut schools: BTreeSet<String> = changed
        .iter()
        .filter_map(|fact| fact.school.clone())
        .collect();
    let athletes: BTreeSet<&str> = changed
        .iter()
        .filter_map(|fact| fact.athlete.as_deref())
        .collect();
    for (members, index) in [
        (&previous.membership, previous.facts.as_ref()),
        (membership, facts),
    ] {
        athletes
            .iter()
            .filter_map(|athlete| members.athletes.get(*athlete))
            .flat_map(|ids| ids.iter())
            .filter_map(|id| index.get(*id))
            .filter_map(|fact| fact.school.clone())
            .for_each(|school| {
                schools.insert(school);
            });
    }
    if previous.auxiliary.as_ref() != auxiliary {
        membership
            .contacts
            .iter()
            .filter_map(|id| facts.get(*id))
            .filter(|fact| {
                fact.authority.as_ref().is_some_and(|origin| {
                    previous.auxiliary.stopped.contains(origin)
                        != auxiliary.stopped.contains(origin)
                }) || fact.school.as_deref().is_some_and(|school| {
                    previous.auxiliary.covers(school, &fact.url)
                        != auxiliary.covers(school, &fact.url)
                })
            })
            .filter_map(|fact| fact.school.clone())
            .for_each(|school| {
                schools.insert(school);
            });
    }
    schools
}

fn merge(
    previous: &[Arc<Candidate>],
    changed: Vec<Arc<Candidate>>,
    schools: &BTreeSet<String>,
) -> Result<Vec<Arc<Candidate>>> {
    let capacity = previous
        .len()
        .checked_add(changed.len())
        .context("candidate capacity overflow")?;
    let mut result = Vec::new();
    result.try_reserve(capacity)?;
    let mut retained = previous
        .iter()
        .filter(|candidate| {
            candidate
                .fact
                .school
                .as_ref()
                .is_none_or(|school| !schools.contains(school))
        })
        .cloned()
        .peekable();
    let mut changed = changed.into_iter().peekable();
    result.extend(std::iter::from_fn(move || {
        match (retained.peek(), changed.peek()) {
            (Some(left), Some(right)) if left.rank <= right.rank => retained.next(),
            (_, Some(_)) => changed.next(),
            _ => retained.next(),
        }
    }));
    Ok(result)
}

pub(super) fn candidates(
    key: &Key,
    facts: &Index<Arc<Fact>>,
    operations: &Index<Arc<Operation>>,
    membership: &Membership,
    auxiliary: &Auxiliary,
    previous: Option<&Prepared>,
    ids: Option<&[i64]>,
) -> Result<Vec<Arc<Candidate>>> {
    let Some((previous, ids)) = previous.zip(ids) else {
        return Ok(compile::compile(key, facts, operations, auxiliary)?
            .into_iter()
            .map(Arc::new)
            .collect());
    };
    let schools = affected(previous, facts, membership, auxiliary, ids);
    let local = membership.local(&schools, facts)?;
    let changed = compile::compile(key, &local, operations, auxiliary)?
        .into_iter()
        .filter(|candidate| {
            candidate
                .fact
                .school
                .as_ref()
                .is_some_and(|school| schools.contains(school))
        })
        .map(Arc::new)
        .collect();
    merge(&previous.candidates, changed, &schools)
}
