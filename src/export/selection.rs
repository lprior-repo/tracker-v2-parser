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
                .any(|selected| selected.eq_ignore_ascii_case(state.trim()))
        });
    let year = record
        .graduation_year
        .is_none_or(|year| options.graduation_years.contains(&year));
    state && year
}

pub(super) fn eligible(record: &AthleteRecord, options: &ExportOptions) -> bool {
    record
        .graduation_year
        .is_some_and(|year| options.graduation_years.contains(&year))
        && record
            .schools
            .iter()
            .filter_map(|school| school.state.as_deref())
            .any(|state| {
                options
                    .states
                    .iter()
                    .any(|selected| selected.eq_ignore_ascii_case(state.trim()))
            })
}

pub(super) fn issues(record: &AthleteRecord) -> Vec<&'static str> {
    let mut issues = Vec::new();
    if record.needs_review {
        issues.push("unresolved identity: needs_review");
    }
    if record.graduation_year.is_none() {
        issues.push("unknown graduation year; excluded from Athletes");
    }
    if !record
        .schools
        .iter()
        .any(|school| published(school.state.as_deref()))
    {
        issues.push("missing published state; excluded from Athletes");
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
    if !record
        .coaches
        .iter()
        .any(|coach| !coach.email.trim().is_empty())
    {
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
        issues.push("non-comparable observed results; see raw records and review_notes");
    }
    issues
}

fn published(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}
