use std::collections::BTreeMap;

use anyhow::Result;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use crate::{
    marks::{self, ComparableMark},
    model::{AthleteRecord, EvidencedPerformance, Source, Sport},
};

type Prs = BTreeMap<String, Vec<String>>;

pub(super) fn prepare(record: &AthleteRecord) -> Result<Prs> {
    let columns = record
        .performances
        .iter()
        .filter(|evidence| evidence.performance.is_personal_record)
        .fold(Prs::new(), |mut columns, evidence| {
            columns
                .entry(column(evidence, &evidence.performance.event, "PR"))
                .or_default()
                .push(display(evidence, false));
            columns
        });
    let mut columns = add_observed(record, columns);
    add_headlines(record, &mut columns)?;
    Ok(columns
        .into_iter()
        .map(|(column, mut values)| {
            values.sort_unstable();
            values.dedup();
            (column, values)
        })
        .collect())
}

fn level(evidence: &EvidencedPerformance) -> &str {
    let level = evidence
        .performance
        .source_values
        .as_ref()
        .and_then(|values| values.get("level"))
        .and_then(Value::as_str)
        .map_or("unspecified", str::trim);
    if level == "all" { "all levels" } else { level }
}

fn column(evidence: &EvidencedPerformance, event: &str, label: &str) -> String {
    let sport = match evidence.performance.sport {
        Sport::Indoor => "Indoor",
        Sport::Outdoor => "Outdoor",
        Sport::CrossCountry => "XC",
    };
    format!(
        "{sport} {event} {label} [{}; {}]",
        level(evidence),
        evidence.source.as_str()
    )
}

fn display(evidence: &EvidencedPerformance, context: bool) -> String {
    let row = &evidence.performance;
    let details = row
        .date
        .iter()
        .cloned()
        .chain(context.then(|| observation_context(evidence)))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if details.is_empty() {
        row.mark.clone()
    } else {
        format!("{} ({details})", row.mark)
    }
}

fn observation_context(evidence: &EvidencedPerformance) -> String {
    let row = &evidence.performance;
    let wind = row.wind.map(|value| format!("wind={value}"));
    let course = (row.sport == Sport::CrossCountry).then(|| {
        row.course
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(
                || {
                    format!(
                        "course unspecified; {}; result={}; {}",
                        row.meet_name.as_deref().map_or("", |s| s),
                        row.result_id
                            .as_deref()
                            .or(row.source_row.as_deref())
                            .map_or("", |s| s),
                        evidence.url
                    )
                },
                |course| format!("course={course}"),
            )
    });
    row.timing
        .iter()
        .cloned()
        .chain(wind)
        .chain(course)
        .chain(marks::event_specification(&row.event).map(|spec| format!("spec={spec}")))
        .collect::<Vec<_>>()
        .join("; ")
}

fn eligible(evidence: &EvidencedPerformance) -> bool {
    !evidence.performance.is_personal_record
        && evidence
            .performance
            .source_values
            .as_ref()
            .and_then(|values| values.get("revision_status"))
            .and_then(Value::as_str)
            != Some("unverified_prior_revision")
}

fn tie_key(evidence: &EvidencedPerformance) -> (String, &str, &str, &str) {
    (
        display(evidence, true),
        evidence.performance.event.as_str(),
        evidence.url.as_str(),
        evidence.performance.result_id.as_deref().map_or("", |s| s),
    )
}

fn add_observed(record: &AthleteRecord, columns: Prs) -> Prs {
    record
        .performances
        .iter()
        .filter(|evidence| eligible(evidence))
        .filter_map(|evidence| {
            marks::comparison(evidence)
                .ok()
                .map(|mark| (evidence, mark))
        })
        .fold(
            BTreeMap::<String, (ComparableMark, &EvidencedPerformance)>::new(),
            |mut bests, (evidence, mark)| {
                let key = format!("{}::level={}", mark.column, level(evidence));
                if bests.get(&key).is_none_or(|(previous, incumbent)| {
                    mark.measure.better(mark.value, previous.value)
                        || (mark.value == previous.value && tie_key(evidence) < tie_key(incumbent))
                }) {
                    bests.insert(key, (mark, evidence));
                }
                bests
            },
        )
        .into_values()
        .fold(columns, |mut columns, (mark, evidence)| {
            let event = mark
                .column
                .split("::")
                .nth(3)
                .map_or(evidence.performance.event.as_str(), |s| s);
            columns
                .entry(column(evidence, event, "best"))
                .or_default()
                .push(display(evidence, true));
            columns
        })
}

fn selector(value: &str) -> Result<Selector> {
    Selector::parse(value).map_err(|error| anyhow::anyhow!("invalid PR selector: {error}"))
}

fn add_headlines(record: &AthleteRecord, columns: &mut Prs) -> Result<()> {
    let row_selector = selector("tr[data-pr-mode='all']")?;
    let event_selector = selector(".pr-td-event")?;
    let mark_selector = selector(".pr-td-mark")?;
    let date_selector = selector(".pr-td-date")?;
    let restricted_selector =
        selector(".mask, .redact, .record--locked, .paywall, [aria-label='Locked']")?;
    record
        .profile_details
        .iter()
        .filter(|profile| profile.source == Source::MileSplit)
        .flat_map(|profile| {
            profile
                .source_values
                .get("pages")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(serde_json::Map::values)
        })
        .flat_map(|page| {
            page.get("pr_rows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|row| row.get("masked").and_then(Value::as_bool) != Some(true))
        .filter_map(|row| row.get("html").and_then(Value::as_str))
        .for_each(|html| {
            let document = Html::parse_fragment(&format!("<table>{html}</table>"));
            document.select(&row_selector).for_each(|row| {
                let text = |selection: &Selector| row_text(row, selection);
                let Some(event) = text(&event_selector).filter(|event| !event.is_empty()) else {
                    return;
                };
                let Some(mark) = text(&mark_selector).filter(|mark| public_mark(mark)) else {
                    return;
                };
                if row.select(&restricted_selector).next().is_some() {
                    return;
                }
                let level = row
                    .value()
                    .attr("data-pr-level")
                    .map_or("unspecified", |s| s);
                let level = if level == "all" { "all levels" } else { level };
                let date = text(&date_selector).filter(|date| date != "-" && !date.is_empty());
                let value = date.map_or_else(
                    || mark.clone(),
                    |date| format!("{mark} ({})", normalized_date(&date)),
                );
                columns
                    .entry(format!("All seasons {event} PR [{level}; milesplit]"))
                    .or_default()
                    .push(value);
            });
        });
    Ok(())
}

fn row_text(row: ElementRef<'_>, selection: &Selector) -> Option<String> {
    row.select(selection).next().map(|element| {
        element
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn public_mark(mark: &str) -> bool {
    mark.chars().any(|ch| ch.is_ascii_digit())
        && !mark.contains('*')
        && marks::failure_status(mark).is_none()
}

fn normalized_date(date: &str) -> String {
    ["%b %e, %Y", "%B %e, %Y", "%Y-%m-%d", "%m/%d/%Y"]
        .iter()
        .find_map(|format| chrono::NaiveDate::parse_from_str(date, format).ok())
        .map_or_else(
            || date.to_owned(),
            |date| date.format("%Y-%m-%d").to_string(),
        )
}
