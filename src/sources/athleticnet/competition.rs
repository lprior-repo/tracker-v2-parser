use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

use crate::model::{CompetitionEvidence, Source, Sport, Target};

use super::values as v;

const MAX_DIVISIONS: usize = 256;
const MAX_EVIDENCE_BYTES: usize = 65_536;

pub(super) fn capture(target: &Target, body: &Value) -> Result<CompetitionEvidence> {
    let sport = target.context.sport.context("competition sport missing")?;
    let key = if sport == Sport::CrossCountry {
        "xcDivisions"
    } else {
        "tfDivisions"
    };
    let divisions = body
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    ensure!(
        divisions.len() <= MAX_DIVISIONS,
        "competition division evidence exceeds limit"
    );
    let evidence = CompetitionEvidence {
        source: Source::AthleticNet,
        meet_id: target
            .context
            .subject_id
            .clone()
            .context("competition meet missing")?,
        sport,
        meet: fields(
            body.get("meet")
                .context("competition meet metadata missing")?,
        ),
        divisions: divisions.iter().map(fields).collect(),
    };
    ensure!(
        serde_json::to_vec(&evidence)?.len() <= MAX_EVIDENCE_BYTES,
        "competition evidence exceeds byte limit"
    );
    Ok(evidence)
}

fn fields(value: &Value) -> Value {
    ["ID", "IDDiv", "MeetID", "Level", "LevelMask"]
        .into_iter()
        .filter_map(|key| value.get(key).map(|value| (key.to_owned(), value.clone())))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

pub(super) fn validated(target: &Target) -> Option<&CompetitionEvidence> {
    let evidence = target.competition_evidence.as_ref()?;
    (target.source == Source::AthleticNet
        && evidence.source == target.source
        && target.context.subject_id.as_deref() == Some(evidence.meet_id.as_str())
        && target.context.sport == Some(evidence.sport)
        && v::id(&evidence.meet, "ID").as_deref() == Some(evidence.meet_id.as_str())
        && evidence.divisions.len() <= MAX_DIVISIONS
        && serde_json::to_vec(evidence).ok()?.len() <= MAX_EVIDENCE_BYTES)
        .then_some(evidence)
}

pub(super) fn level(value: &Value) -> Option<bool> {
    if value.get("LevelMask").is_some() {
        return Some(false);
    }
    if let Some(level) = value.get("Level") {
        return Some(
            level.as_u64() == Some(4)
                || level.as_str().is_some_and(|level| {
                    ["hs", "high school", "high_school", "highschool"]
                        .into_iter()
                        .any(|name| level.trim().eq_ignore_ascii_case(name))
                }),
        );
    }
    None
}

pub(super) fn result_level(
    evidence: Option<&CompetitionEvidence>,
    block: &Value,
    row: &Value,
) -> Option<&'static str> {
    let published = [
        level(row),
        level(block),
        evidence.and_then(|evidence| inherited_level(evidence, block, row)),
    ];
    (published.contains(&Some(true)) && !published.contains(&Some(false))).then_some("high school")
}

fn inherited_level(evidence: &CompetitionEvidence, block: &Value, row: &Value) -> Option<bool> {
    if row.get("MeetID").is_some()
        && v::id(row, "MeetID").as_deref() != Some(evidence.meet_id.as_str())
    {
        return Some(false);
    }
    if evidence.divisions.is_empty() {
        return level(&evidence.meet);
    }
    let row_division = v::id(row, "IDDiv");
    let block_division = v::id(block, "DivId");
    if row_division.is_some() && block_division.is_some() && row_division != block_division {
        return Some(false);
    }
    let division_id = row_division.or(block_division)?;
    let division = evidence
        .divisions
        .iter()
        .filter(|division| v::id(division, "IDDiv").as_deref() == Some(division_id.as_str()))
        .try_fold(None, |found, division| {
            if found.is_some() {
                Err(())
            } else {
                Ok(Some(division))
            }
        })
        .ok()??;
    if division.get("MeetID").is_some()
        && v::id(division, "MeetID").as_deref() != Some(evidence.meet_id.as_str())
    {
        return Some(false);
    }
    level(division).or_else(|| level(&evidence.meet))
}
