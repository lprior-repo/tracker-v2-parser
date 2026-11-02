use std::{cmp::Reverse, sync::Arc};

use super::{index::Index, membership::Membership};

use anyhow::{Result, ensure};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub states: Vec<String>,
    pub years: Vec<i32>,
    pub generation: Option<String>,
    pub max_attempts: i32,
    pub contacts: bool,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct Fact {
    pub id: i64,
    pub status: String,
    pub attempts: i32,
    pub url: String,
    pub profile_id: Option<i64>,
    pub athlete_id: Option<i64>,
    pub school_id: Option<i64>,
    pub stage: i16,
    pub section: i16,
    pub athlete: Option<String>,
    pub school: Option<String>,
    pub valid: bool,
    pub known_school: bool,
    pub official: bool,
    pub authority: Option<String>,
}

impl Fact {
    pub fn retained_bytes(&self) -> Result<usize> {
        [
            Some(self.url.as_str()),
            Some(self.status.as_str()),
            self.school.as_deref(),
            self.athlete.as_deref(),
            self.authority.as_deref(),
        ]
        .into_iter()
        .flatten()
        .try_fold(std::mem::size_of::<Self>(), |total, value| {
            total
                .checked_add(value.len())
                .ok_or_else(|| anyhow::anyhow!("compiled coverage size overflow"))
        })
    }

    pub fn pending(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "failed")
    }

    pub fn complete(&self) -> bool {
        self.status == "succeeded"
    }

    pub fn section_bit(&self) -> Result<u8> {
        ensure!(
            (0..=2).contains(&self.section),
            "invalid compiled profile section"
        );
        Ok(1_u8
            .checked_shl(u32::try_from(self.section)?)
            .map_or(0, |bit| bit))
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub(super) struct Operation {
    pub identity: String,
    pub page_id: i64,
    pub state: String,
    pub cycle: i32,
    pub cycle_attempts: i32,
    pub retry_at_ms: Option<i64>,
}

impl Operation {
    pub fn terminal(&self) -> bool {
        matches!(
            self.state.as_str(),
            "completed" | "blocked" | "rejected" | "exhausted"
        )
    }

    pub fn effort(&self) -> Result<i64> {
        i64::from(self.cycle)
            .checked_sub(1)
            .and_then(|cycle| cycle.checked_mul(3))
            .and_then(|attempts| attempts.checked_add(i64::from(self.cycle_attempts)))
            .ok_or_else(|| anyhow::anyhow!("compiled retry priority overflow"))
    }
}

pub(super) type Rank = (Reverse<bool>, i64, i64, i16, i64, i16, Reverse<bool>, i64);

#[derive(Debug)]
pub(super) struct Candidate {
    pub fact: Arc<Fact>,
    pub operation: Option<Arc<Operation>>,
    pub rank: Rank,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Auxiliary {
    pub stopped: std::collections::HashSet<String>,
    pub official: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl Auxiliary {
    pub fn covers(&self, school: &str, url: &str) -> bool {
        self.official
            .get(school)
            .is_some_and(|urls| urls.contains(url))
    }
}

#[derive(Debug)]
pub(super) struct Snapshot {
    pub key: Key,
    pub visibility: String,
    pub prepared: Arc<Prepared>,
}

#[derive(Debug)]
pub(super) struct Prepared {
    pub facts: Arc<Index<Arc<Fact>>>,
    pub operations: Arc<Index<Arc<Operation>>>,
    pub auxiliary: Arc<Auxiliary>,
    pub candidates: Vec<Arc<Candidate>>,
    pub membership: Membership,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct Owned {
    pub id: i64,
    pub source: String,
    pub kind: Option<String>,
    pub school: Option<String>,
    pub athlete: Option<String>,
}

pub(super) fn conflicts(candidate: &Fact, owned: &Owned) -> bool {
    if owned.source != "milesplit" {
        return false;
    }
    let same_athlete = candidate
        .athlete
        .as_ref()
        .is_some_and(|id| owned.athlete.as_ref() == Some(id));
    let same_school = candidate
        .school
        .as_ref()
        .is_some_and(|id| owned.school.as_ref() == Some(id));
    owned.id == candidate.id
        || (candidate.stage == 2 && same_athlete)
        || (same_school
            && (matches!(owned.kind.as_deref(), Some("team" | "roster"))
                || matches!(candidate.stage, 0 | 1 | 3)
                || (candidate.stage == 2 && same_athlete)))
}

impl Snapshot {
    pub fn selected(
        &self,
        now_ms: i64,
        allow_deferred: bool,
        owned: &[Owned],
        exhausted: bool,
    ) -> Option<i64> {
        let first = self.prepared.candidates.iter().find(|candidate| {
            let deferred = candidate
                .operation
                .as_ref()
                .filter(|operation| operation.state == "deferred");
            (allow_deferred || deferred.is_none())
                && deferred
                    .is_none_or(|operation| operation.retry_at_ms.is_none_or(|due| due <= now_ms))
                && !owned.iter().any(|page| conflicts(&candidate.fact, page))
        })?;
        (self.key.generation.is_some()
            || (first.fact.attempts >= self.key.max_attempts) == exhausted)
            .then_some(first.fact.id)
    }
}
