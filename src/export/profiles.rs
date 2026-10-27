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
        .all(|record| record.profile_details.is_empty())
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
        record.profile_details.iter().flat_map(move |profile| {
            let serialized = profile.source_values.to_string();
            chunks(&serialized)
                .enumerate()
                .map(|(part, chunk)| {
                    vec![
                        record.id.to_string(),
                        record.name.clone(),
                        profile.source.as_str().to_owned(),
                        profile.source_id.clone(),
                        profile
                            .url
                            .clone()
                            .map_or(String::new(), core::convert::identity),
                        part.saturating_add(1).to_string(),
                        chunk.to_owned(),
                    ]
                })
                .collect::<Vec<_>>()
        })
    });
    capacity::write(workbook, "Profile_details", &headers, rows, true, expected)
}

fn chunks(value: &str) -> impl Iterator<Item = &str> {
    let mut remaining = value;
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        let boundary = remaining.floor_char_boundary(8_000);
        let (chunk, tail) = remaining.split_at(boundary);
        remaining = tail;
        Some(chunk)
    })
}
