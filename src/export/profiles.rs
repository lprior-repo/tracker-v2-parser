use anyhow::Result;
use rust_xlsxwriter::Workbook;

use super::capacity;
use crate::model::AthleteRecord;

pub(super) fn write(
    workbook: &mut Workbook,
    records: &[&AthleteRecord],
    expected: &mut Vec<capacity::SheetCount>,
) -> Result<()> {
    if records
        .iter()
        .all(|record| record.profile_details.is_empty() && record.profiles.is_empty())
    {
        return Ok(());
    }
    let headers = [
        "athlete_id",
        "name",
        "source",
        "source_athlete_id",
        "profile_url",
        "part",
        "public_source_json",
    ]
    .map(str::to_owned);
    let rows = records.iter().flat_map(|record| {
        let details = record
            .profile_details
            .iter()
            .flat_map(move |profile| profile_rows(record, profile));
        let links = record
            .profiles
            .iter()
            .filter(|url| {
                !record
                    .profile_details
                    .iter()
                    .any(|profile| profile.url.as_deref() == Some(url.as_str()))
            })
            .map(|url| {
                Ok(vec![
                    record.id.to_string(),
                    record.name.clone(),
                    String::new(),
                    String::new(),
                    url.clone(),
                    "1".to_owned(),
                    String::new(),
                ])
            });
        details.chain(links)
    });
    capacity::write_fallible(workbook, "Profile_details", &headers, rows, true, expected)
}

fn profile_rows(
    record: &AthleteRecord,
    profile: &crate::model::AthleteProfileDetails,
) -> Vec<Result<Vec<String>>> {
    let serialized = match crate::transport::redact_public_json(&profile.source_values) {
        Ok(serialized) => serialized,
        Err(error) => return vec![Err(error)],
    };
    capacity::chunks(&serialized)
        .enumerate()
        .map(|(part, chunk)| {
            Ok(vec![
                record.id.to_string(),
                if part == 0 {
                    record.name.clone()
                } else {
                    String::new()
                },
                profile.source.as_str().to_owned(),
                if part == 0 {
                    profile.source_id.clone()
                } else {
                    String::new()
                },
                profile
                    .url
                    .clone()
                    .map_or(String::new(), core::convert::identity),
                part.saturating_add(1).to_string(),
                chunk.to_owned(),
            ])
        })
        .collect()
}
