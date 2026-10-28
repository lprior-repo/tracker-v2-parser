use super::ExportOptions;
use crate::model::AthleteRecord;

pub(super) fn in_scope(record: &AthleteRecord, options: &ExportOptions) -> bool {
    let mut states = record
        .schools
        .iter()
        .filter_map(|school| school.state.as_deref())
        .filter(|state| !state.trim().is_empty())
        .peekable();
    let state = states.peek().is_none()
        || states.any(|state| {
            options
                .states
                .iter()
                .any(|selected| selected.trim().eq_ignore_ascii_case(state.trim()))
        });
    let year = record
        .graduation_year
        .is_none_or(|year| options.graduation_years.contains(&year));
    state && year
}

pub(super) fn requires_review(record: &AthleteRecord) -> bool {
    record.needs_review
        || record.name.trim().is_empty()
        || record.graduation_year.is_none()
        || !record
            .schools
            .iter()
            .any(|school| published(school.state.as_deref()))
        || missing_coach_name(record)
        || missing_coach_email(record)
        || record
            .coaches
            .iter()
            .any(|coach| !coach.review_notes.is_empty())
}

pub(super) fn issues(record: &AthleteRecord) -> Vec<&'static str> {
    let mut issues = Vec::new();
    if record.needs_review {
        issues.push("unresolved identity: needs_review");
    }
    if record.graduation_year.is_none() {
        issues.push("unknown graduation year; source row retained for review");
    }
    if !record
        .schools
        .iter()
        .any(|school| published(school.state.as_deref()))
    {
        issues.push("missing published state; source row retained for review");
    }
    if record.name.trim().is_empty() {
        issues.push("missing athlete name");
    }
    if !published(record.gender.as_deref()) {
        issues.push("missing published gender");
    }
    if !published(record.grade.as_deref()) {
        issues.push("missing published grade");
    }
    if !record
        .profiles
        .iter()
        .any(|profile| !profile.trim().is_empty())
    {
        issues.push("missing source profiles");
    }
    if record.schools.is_empty() {
        issues.push("missing published school");
    }
    if !record
        .schools
        .iter()
        .any(|school| published(school.address.as_deref()))
    {
        issues.push("missing published school postal address");
    }
    if !record
        .schools
        .iter()
        .any(|school| published(school.public_email.as_deref()))
    {
        issues.push("missing published school email");
    }
    if missing_coach_name(record) {
        issues.push("missing published coach name");
    }
    if missing_coach_email(record) {
        issues.push("missing public coach email");
    }
    if record.performances.is_empty() {
        issues.push("missing observed performances");
    }
    if record
        .performances
        .iter()
        .any(|evidence| crate::marks::comparable(evidence).is_none())
    {
        issues.push("non-comparable observed results; see Issues and Observations");
    }
    issues
}

pub(super) fn contact_issues(record: &AthleteRecord) -> impl Iterator<Item = String> + '_ {
    record.coaches.iter().flat_map(|coach| {
        coach.review_notes.iter().map(move |note| {
            format!(
                "contact {} <{}> | school_id={} | {}: {note}",
                coach.name.as_deref().map_or("unnamed", |name| name),
                coach
                    .email
                    .as_deref()
                    .map_or("unpublished email", |email| email),
                coach.school_id,
                coach.role
            )
        })
    })
}

pub(super) fn hs_comparison(
    evidence: &crate::model::EvidencedPerformance,
) -> Result<crate::marks::ComparableMark, &'static str> {
    if evidence
        .performance
        .source_values
        .as_ref()
        .and_then(|values| values.get("revision_status"))
        .and_then(serde_json::Value::as_str)
        == Some("unverified_prior_revision")
    {
        return Err(
            "unverified prior revision; not reconfirmed in the latest observed source file",
        );
    }
    let level = evidence
        .performance
        .source_values
        .as_ref()
        .and_then(|values| values.get("level"))
        .and_then(serde_json::Value::as_str);
    if !level.is_some_and(is_hs) {
        return Err("not explicitly published as high school");
    }
    let attributes = evidence
        .performance
        .source_values
        .as_ref()
        .and_then(|values| values.pointer("/row/attributes"));
    if ["data-level", "data-pr-level"].into_iter().any(|key| {
        attributes
            .and_then(|values| values.get(key))
            .is_some_and(|value| !value.as_str().is_some_and(is_hs))
    }) {
        return Err("conflicting published competition levels");
    }
    crate::marks::comparison(evidence)
}

fn is_hs(level: &str) -> bool {
    ["hs", "high school", "high_school", "highschool"]
        .into_iter()
        .any(|candidate| level.trim().eq_ignore_ascii_case(candidate))
}

fn missing_coach_name(record: &AthleteRecord) -> bool {
    record.coaches.is_empty()
        || record
            .coaches
            .iter()
            .any(|coach| !published(coach.name.as_deref()))
}

fn missing_coach_email(record: &AthleteRecord) -> bool {
    record.coaches.is_empty()
        || record
            .coaches
            .iter()
            .any(|coach| !published(coach.email.as_deref()))
}

fn published(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}
