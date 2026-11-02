use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Result;

use super::index::Index;
use super::model::{Auxiliary, Candidate, Fact, Key, Operation};

#[derive(Default)]
struct Athlete {
    first: i64,
    completed: u8,
}

struct Groups<'a> {
    athletes: HashMap<&'a str, Athlete>,
    known: HashSet<&'a str>,
    schools: HashMap<&'a str, i64>,
    completed: HashSet<(&'a str, i16, &'a str)>,
    official: HashSet<&'a str>,
}

fn parents<'a>(
    facts: &'a Index<Arc<Fact>>,
    key: &Key,
    auxiliary: &Auxiliary,
    known: &HashSet<&str>,
) -> Vec<&'a Arc<Fact>> {
    facts
        .values()
        .filter(|fact| {
            fact.stage != 2
                && fact.valid
                && fact
                    .school
                    .as_deref()
                    .is_some_and(|school| known.contains(school))
                && (fact.stage != 3
                    || (key.contacts
                        && fact
                            .authority
                            .as_ref()
                            .is_none_or(|origin| !auxiliary.stopped.contains(origin))))
        })
        .collect()
}

fn groups<'a>(facts: &'a Index<Arc<Fact>>, key: &Key, auxiliary: &Auxiliary) -> Result<Groups<'a>> {
    let mut groups = Groups {
        athletes: HashMap::new(),
        known: HashSet::new(),
        schools: HashMap::new(),
        completed: HashSet::new(),
        official: HashSet::new(),
    };
    facts
        .values()
        .filter(|fact| fact.valid && fact.stage == 2)
        .try_for_each(|fact| -> Result<()> {
            if let (Some(athlete), Some(school)) = (fact.athlete.as_deref(), fact.school.as_deref())
            {
                let summary = groups.athletes.entry(athlete).or_insert(Athlete {
                    first: fact.id,
                    completed: 0,
                });
                summary.first = summary.first.min(fact.id);
                if fact.complete() {
                    summary.completed |= fact.section_bit()?;
                }
                groups.known.insert(school);
                groups
                    .schools
                    .entry(school)
                    .and_modify(|id| *id = (*id).min(fact.id))
                    .or_insert(fact.id);
            }
            Ok(())
        })?;
    facts
        .values()
        .filter(|fact| fact.known_school)
        .filter_map(|fact| fact.school.as_deref())
        .for_each(|school| {
            groups.known.insert(school);
        });
    let groups = parents(facts, key, auxiliary, &groups.known)
        .into_iter()
        .fold(groups, |mut groups, fact| {
            if let Some(school) = fact.school.as_deref() {
                groups
                    .schools
                    .entry(school)
                    .and_modify(|id| *id = (*id).min(fact.id))
                    .or_insert(fact.id);
                if fact.complete() {
                    groups.completed.insert((
                        school,
                        fact.stage,
                        if fact.stage == 3 {
                            fact.url.as_str()
                        } else {
                            ""
                        },
                    ));
                    if fact.official && auxiliary.covers(school, &fact.url) {
                        groups.official.insert(school);
                    }
                }
            }
            groups
        });
    Ok(groups)
}

fn eligible(
    fact: &Fact,
    key: &Key,
    auxiliary: &Auxiliary,
    groups: &Groups<'_>,
) -> Result<Option<i64>> {
    if !fact.pending() || !fact.valid {
        return Ok(None);
    }
    let Some(school) = fact
        .school
        .as_deref()
        .filter(|school| groups.schools.contains_key(school))
    else {
        return Ok(None);
    };
    if fact.stage == 2 {
        let Some(athlete) = fact
            .athlete
            .as_deref()
            .and_then(|athlete| groups.athletes.get(athlete))
        else {
            return Ok(None);
        };
        return Ok((athlete.completed & fact.section_bit()? == 0
            && (fact.section == 0 || athlete.completed & 1 != 0))
            .then_some(athlete.first));
    }
    if !groups.known.contains(school) {
        return Ok(None);
    }
    if fact.stage == 3
        && (!key.contacts
            || (key.generation.is_none() && fact.attempts >= key.max_attempts)
            || fact
                .authority
                .as_ref()
                .is_some_and(|origin| auxiliary.stopped.contains(origin))
            || (!fact.official && groups.official.contains(school)))
    {
        return Ok(None);
    }
    let duplicate = groups.completed.contains(&(
        school,
        fact.stage,
        if fact.stage == 3 {
            fact.url.as_str()
        } else {
            ""
        },
    ));
    Ok((!duplicate).then_some(0))
}

pub(super) fn compile(
    key: &Key,
    facts: &Index<Arc<Fact>>,
    operations: &Index<Arc<Operation>>,
    auxiliary: &Auxiliary,
) -> Result<Vec<Candidate>> {
    let groups = groups(facts, key, auxiliary)?;
    let mut candidates = Vec::new();
    candidates.try_reserve(facts.len())?;
    facts.values().try_for_each(|fact| -> Result<()> {
        let Some(athlete_order) = eligible(fact, key, auxiliary, &groups)? else {
            return Ok(());
        };
        let operation = operations.get(fact.id);
        if operation.is_some_and(|operation| operation.terminal()) {
            return Ok(());
        }
        let school_order = fact
            .school
            .as_deref()
            .and_then(|school| groups.schools.get(school))
            .copied()
            .map_or(fact.id, |order| order);
        let section = if fact.stage == 2 {
            fact.section
        } else {
            i16::from(!fact.official || fact.stage != 3)
        };
        let rank = (
            Reverse(operation.is_some_and(|operation| operation.state == "active")),
            operation
                .map(|operation| operation.effort())
                .transpose()?
                .map_or(0, |effort| effort),
            school_order,
            fact.stage,
            athlete_order,
            section,
            Reverse(fact.attempts == 0),
            fact.id,
        );
        candidates.push(Candidate {
            fact: Arc::clone(fact),
            operation: operation.cloned(),
            rank,
        });
        Ok(())
    })?;
    candidates.sort_unstable_by_key(|candidate| candidate.rank);
    Ok(candidates)
}
