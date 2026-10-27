use crate::marks::{ComparableMark, comparable};
use crate::model::{AthleteRecord, EvidencedPerformance};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const BASE_HEADERS: [&str; 16] = [
    "athlete_id",
    "name",
    "gender",
    "grade",
    "graduation_year",
    "source_profiles",
    "school_names",
    "school_uuids",
    "school_states",
    "school_postal_addresses",
    "school_public_emails",
    "coach_emails",
    "identity_coverage_caveat",
    "unmapped_count",
    "review_notes",
    "needs_review",
];

pub(super) struct Prepared<'a> {
    pub bests: BTreeMap<String, (ComparableMark, &'a EvidencedPerformance)>,
    unmapped: usize,
    notes: BTreeSet<String>,
}

pub(super) fn prepare(record: &AthleteRecord) -> Prepared<'_> {
    record.performances.iter().fold(
        Prepared {
            bests: BTreeMap::new(),
            unmapped: 0,
            notes: BTreeSet::new(),
        },
        |mut row, evidence| {
            let Some(mark) = comparable(evidence) else {
                row.unmapped = row.unmapped.saturating_add(1);
                row.notes.insert(format!(
                    "not comparable: {} | {} | {}",
                    evidence.performance.event, evidence.performance.mark, evidence.url
                ));
                return row;
            };
            let replace = row
                .bests
                .get(&mark.column)
                .is_none_or(|(incumbent, previous)| {
                    mark.measure.better(mark.value, incumbent.value)
                        || (mark.value == incumbent.value && tie_key(evidence) < tie_key(previous))
                });
            if replace {
                row.bests.insert(mark.column.clone(), (mark, evidence));
            }
            row
        },
    )
}

fn tie_key(evidence: &EvidencedPerformance) -> (&str, &str, &str, &str) {
    (
        evidence
            .performance
            .date
            .as_deref()
            .map_or("9999-99-99", |date| date),
        evidence
            .performance
            .result_id
            .as_deref()
            .map_or("", |id| id),
        evidence.url.as_str(),
        evidence.performance.mark.as_str(),
    )
}

pub(super) fn cells(
    record: &AthleteRecord,
    row: &Prepared<'_>,
    columns: &BTreeSet<String>,
) -> Vec<String> {
    let base = vec![
        record.id.to_string(),
        record.name.clone(),
        optional(record.gender.as_deref()),
        optional(record.grade.as_deref()),
        record
            .graduation_year
            .map_or_else(String::new, |year| year.to_string()),
        joined(record.profiles.iter().map(String::as_str)),
        joined(record.schools.iter().map(|school| school.name.as_str())),
        joined(
            record
                .schools
                .iter()
                .filter_map(|school| school.uuid.as_deref()),
        ),
        joined(
            record
                .schools
                .iter()
                .filter_map(|school| school.state.as_deref()),
        ),
        addresses(record),
        joined(
            record
                .schools
                .iter()
                .filter_map(|school| school.public_email.as_deref()),
        ),
        joined(record.coaches.iter().map(|coach| coach.email.as_str())),
        caveat(record),
        row.unmapped.to_string(),
        joined(row.notes.iter().map(String::as_str)),
        record.needs_review.to_string(),
    ];
    base.into_iter()
        .chain(columns.iter().flat_map(|column| {
            row.bests.get(column).map_or_else(
                || [String::new(), String::new(), String::new()],
                |(_, evidence)| {
                    [
                        evidence.performance.mark.clone(),
                        evidence.url.clone(),
                        evidence.performance.event.clone(),
                    ]
                },
            )
        }))
        .collect()
}

fn optional(value: Option<&str>) -> String {
    value.map_or_else(String::new, str::to_owned)
}

fn joined<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .filter(|value| !value.trim().is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("; ")
}

fn addresses(record: &AthleteRecord) -> String {
    record
        .schools
        .iter()
        .filter_map(|school| {
            let address = joined(
                [
                    school.address.as_deref(),
                    school.city.as_deref(),
                    school.state.as_deref(),
                    school.postal_code.as_deref(),
                ]
                .into_iter()
                .flatten(),
            );
            (!address.is_empty()).then(|| format!("{}: {address}", school.name))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("; ")
}

fn caveat(record: &AthleteRecord) -> String {
    let schools = record
        .schools
        .iter()
        .map(|school| school.name.as_str())
        .collect::<BTreeSet<_>>();
    let affiliation = if schools.len() > 1 {
        " Multiple published schools; current affiliation unresolved."
    } else {
        ""
    };
    format!(
        "Best observed in captured pages, not verified complete lifetime PRs. Identity uses source IDs and explicit sameAs only; separate unlinked profiles may describe the same person. Source, timing, wind, event specification and XC course contexts remain separate; unspecified XC courses are result-scoped and not ranked across results.{affiliation}"
    )
}
