mod event;
mod notation;

use crate::model::{EvidencedPerformance, Performance, Source, Sport};
pub use notation::{Measure, parse_mark};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparableMark {
    pub column: String,
    pub value: i64,
    pub measure: Measure,
}

#[must_use]
pub fn comparable(evidence: &EvidencedPerformance) -> Option<ComparableMark> {
    comparable_performance(evidence.source, &evidence.url, &evidence.performance)
}

pub fn comparison(evidence: &EvidencedPerformance) -> Result<ComparableMark, &'static str> {
    comparison_performance(evidence.source, &evidence.url, &evidence.performance)
}

#[must_use]
pub fn event_specification(label: &str) -> Option<String> {
    event::resolve(label)?.specification
}

#[must_use]
pub fn normalized_mark(label: &str, raw: &str) -> Option<(i64, Measure, Option<String>)> {
    let event = event::resolve(label)?;
    if event.relay {
        return None;
    }
    let value = parse_mark(raw, event.measure)?;
    Some((value, event.measure, event.specification))
}

#[must_use]
pub fn normalized_performance(performance: &Performance) -> Option<(i64, Measure, Option<String>)> {
    if performance.is_relay
        || performance
            .reported_status
            .as_deref()
            .and_then(failure_status)
            .is_some()
    {
        return None;
    }
    normalized_mark(&performance.event, &performance.mark)
}

pub(crate) fn failure_status(value: &str) -> Option<&'static str> {
    [
        "DQ", "DNF", "DNS", "NH", "NM", "NT", "SCR", "FS", "FOUL", "DNQ", "WD", "RET",
    ]
    .into_iter()
    .find(|status| value.trim().eq_ignore_ascii_case(status))
}

#[must_use]
pub fn comparable_performance(
    source: Source,
    url: &str,
    performance: &Performance,
) -> Option<ComparableMark> {
    comparison_performance(source, url, performance).ok()
}

fn comparison_performance(
    source: Source,
    url: &str,
    performance: &Performance,
) -> Result<ComparableMark, &'static str> {
    let event = event::resolve(&performance.event).ok_or("unsupported event")?;
    if performance.is_relay || event.relay {
        return Err("team relay, not an individual split");
    }
    if performance
        .reported_status
        .as_deref()
        .and_then(failure_status)
        .is_some()
    {
        return Err("source-reported non-finish or disqualification");
    }
    if !event::has_comparable_specification(&performance.event, &event.label) {
        return Err("missing or unsupported implement/hurdle specification");
    }
    let value =
        parse_mark(&performance.mark, event.measure).ok_or("unsupported or non-finish mark")?;
    let timing = notation::timing(
        &performance.mark,
        performance.timing.as_deref(),
        event.measure,
    );
    let wind = classify_wind(performance.sport, event.wind_sensitive, performance.wind);
    let mut column = format!(
        "best_observed::{}::{}::{}::{timing}::{wind}",
        source.as_str(),
        performance.sport.as_str(),
        event.label
    );
    if let Some(specification) = event.specification {
        column.push_str("::spec=");
        column.push_str(&specification);
    }
    if performance.sport == Sport::CrossCountry {
        column.push_str("::course=");
        match performance
            .course
            .as_deref()
            .filter(|course| !course.trim().is_empty())
        {
            Some(course) => column.push_str(course.trim()),
            None => append_unknown_course(&mut column, url, performance),
        }
    }
    Ok(ComparableMark {
        column,
        value,
        measure: event.measure,
    })
}

fn append_unknown_course(column: &mut String, url: &str, performance: &Performance) {
    column.push_str("unknown-result@");
    column.push_str(url);
    for part in [
        performance.result_id.as_deref(),
        performance.source_row.as_deref(),
        performance.meet_id.as_deref(),
        performance.date.as_deref(),
        performance.meet_name.as_deref(),
        Some(performance.mark.as_str()),
    ] {
        column.push_str("::");
        column.push_str(part.map_or("", |value| value));
    }
}

fn classify_wind(sport: Sport, sensitive: bool, wind: Option<f64>) -> &'static str {
    if sport != Sport::Outdoor || !sensitive {
        return "na";
    }
    match wind {
        Some(value) if value.is_finite() && value <= 2.0 => "legal",
        Some(value) if value.is_finite() => "assisted",
        _ => "unknown",
    }
}

#[cfg(test)]
pub(crate) mod tests;
