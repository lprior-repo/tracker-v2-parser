use serde_json::Value;
use track_scrapper_v2::model::{AthleteProfileDetails, AthleteRecord, Source};

fn profile_for_sheet(mut profile: AthleteProfileDetails) -> AthleteProfileDetails {
    let pages = if profile.source == Source::MileSplit {
        profile
            .source_values
            .get("pages")
            .and_then(Value::as_object)
    } else {
        None
    };
    let pages = pages
        .into_iter()
        .flatten()
        .filter_map(|(url, page)| {
            let rows = page.get("pr_rows")?.as_array()?;
            let rows = rows
                .iter()
                .filter_map(|row| {
                    let html = row.get("html")?.as_str()?;
                    Some(serde_json::json!({"html": html, "masked": row.get("masked")}))
                })
                .collect::<Vec<_>>();
            if rows.is_empty() {
                None
            } else {
                Some((url.clone(), serde_json::json!({"pr_rows": rows})))
            }
        })
        .collect::<serde_json::Map<String, Value>>();
    profile.source_values = serde_json::json!({"pages": pages});
    profile
}

fn performance_values(values: Option<Value>) -> Option<Value> {
    values.and_then(|values| {
        let fields = ["level", "revision_status"]
            .into_iter()
            .filter_map(|field| {
                values
                    .get(field)
                    .map(|value| (field.to_owned(), value.clone()))
            })
            .collect::<serde_json::Map<String, Value>>();
        (!fields.is_empty()).then_some(Value::Object(fields))
    })
}

pub fn record_for_sheet(mut record: AthleteRecord) -> AthleteRecord {
    record.profile_details = record
        .profile_details
        .into_iter()
        .map(profile_for_sheet)
        .collect();
    record.performances = record
        .performances
        .into_iter()
        .map(|mut evidence| {
            evidence.performance.source_values =
                performance_values(evidence.performance.source_values);
            evidence
        })
        .collect();
    record
}
