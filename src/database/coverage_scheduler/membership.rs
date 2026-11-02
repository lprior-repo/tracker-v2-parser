use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::Result;

use super::{index::Index, model::Fact};

type Groups = BTreeMap<Arc<str>, Arc<BTreeSet<i64>>>;

#[derive(Clone, Debug, Default)]
pub(super) struct Membership {
    pub schools: Groups,
    pub athletes: Groups,
    pub contacts: Arc<BTreeSet<i64>>,
}

fn insert(groups: &mut Groups, key: Option<&str>, id: i64) {
    if let Some(key) = key {
        Arc::make_mut(groups.entry(Arc::from(key)).or_default()).insert(id);
    }
}

fn remove(groups: &mut Groups, key: Option<&str>, id: i64) {
    if let Some(key) = key
        && let Some(ids) = groups.get_mut(key)
    {
        Arc::make_mut(ids).remove(&id);
        if ids.is_empty() {
            groups.remove(key);
        }
    }
}

impl Membership {
    fn add(&mut self, fact: &Fact) {
        insert(&mut self.schools, fact.school.as_deref(), fact.id);
        insert(&mut self.athletes, fact.athlete.as_deref(), fact.id);
        if fact.stage == 3 {
            Arc::make_mut(&mut self.contacts).insert(fact.id);
        }
    }

    fn delete(&mut self, fact: &Fact) {
        remove(&mut self.schools, fact.school.as_deref(), fact.id);
        remove(&mut self.athletes, fact.athlete.as_deref(), fact.id);
        if fact.stage == 3 {
            Arc::make_mut(&mut self.contacts).remove(&fact.id);
        }
    }

    pub fn updated(
        previous: Option<(&Self, &Index<Arc<Fact>>)>,
        facts: &Index<Arc<Fact>>,
        ids: Option<&[i64]>,
    ) -> Self {
        let Some(((previous, old), ids)) = previous.zip(ids) else {
            let mut result = Self::default();
            facts.values().for_each(|fact| result.add(fact));
            return result;
        };
        let mut result = previous.clone();
        for id in ids {
            let before = old.get(*id);
            let after = facts.get(*id);
            if before.map(|fact| (&fact.school, &fact.athlete, fact.stage))
                == after.map(|fact| (&fact.school, &fact.athlete, fact.stage))
            {
                continue;
            }
            if let Some(fact) = before {
                result.delete(fact);
            }
            if let Some(fact) = after {
                result.add(fact);
            }
        }
        result
    }

    pub fn local(
        &self,
        schools: &BTreeSet<String>,
        facts: &Index<Arc<Fact>>,
    ) -> Result<Index<Arc<Fact>>> {
        let mut ids: BTreeSet<i64> = schools
            .iter()
            .filter_map(|school| self.schools.get(school.as_str()))
            .flat_map(|ids| ids.iter().copied())
            .collect();
        let athletes: BTreeSet<&str> = ids
            .iter()
            .filter_map(|id| facts.get(*id))
            .filter_map(|fact| fact.athlete.as_deref())
            .collect();
        athletes
            .into_iter()
            .filter_map(|athlete| self.athletes.get(athlete))
            .for_each(|related| ids.extend(related.iter().copied()));
        ids.into_iter().filter_map(|id| facts.get(id)).try_fold(
            Index::default(),
            |mut index, fact| {
                index.insert(fact.id, Arc::clone(fact), fact.retained_bytes()?)?;
                Ok(index)
            },
        )
    }
}
