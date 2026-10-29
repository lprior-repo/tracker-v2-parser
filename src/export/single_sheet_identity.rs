use std::collections::BTreeSet;

use super::super::rows;
use crate::model::{AthleteRecord, Coach, Sport};

pub(super) fn identity(record: &AthleteRecord) -> Vec<String> {
    vec![
        record.name.clone(),
        record
            .gender
            .as_deref()
            .map_or_else(String::new, str::to_owned),
        record
            .graduation_year
            .map_or_else(String::new, |year| year.to_string()),
        rows::joined(record.schools.iter().map(|school| school.name.as_str())),
        school_addresses(record),
        coach_field(record, Program::Track, CoachField::Name),
        coach_field(record, Program::Track, CoachField::Email),
        coach_field(record, Program::CrossCountry, CoachField::Name),
        coach_field(record, Program::CrossCountry, CoachField::Email),
        rows::joined(
            record
                .schools
                .iter()
                .filter_map(|school| school.public_email.as_deref()),
        ),
        rows::joined(record.profiles.iter().map(String::as_str)),
        record.id.to_string(),
        coach_field(record, Program::Unspecified, CoachField::Name),
        coach_field(record, Program::Unspecified, CoachField::Email),
    ]
}

fn school_addresses(record: &AthleteRecord) -> String {
    record
        .schools
        .iter()
        .filter_map(|school| {
            let street = school
                .address
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            let city = school
                .city
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let region = [school.state.as_deref(), school.postal_code.as_deref()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let address = std::iter::once(street)
                .chain(city)
                .chain((!region.is_empty()).then_some(region.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("{}: {address}", school.name))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Clone, Copy)]
enum Program {
    Track,
    CrossCountry,
    Unspecified,
}

#[derive(Clone, Copy)]
enum CoachField {
    Name,
    Email,
}

fn coach_field(record: &AthleteRecord, program: Program, field: CoachField) -> String {
    record
        .coaches
        .iter()
        .filter(|coach| matches_program(coach, program))
        .map(|coach| {
            (
                coach.name.as_deref().map_or("", str::trim),
                coach.email.as_deref().map_or("", str::trim),
            )
        })
        .filter(|(name, email)| !name.is_empty() || !email.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|(name, email)| match field {
            CoachField::Name => name,
            CoachField::Email => email,
        })
        .map(|value| if value.is_empty() { "—" } else { value })
        .collect::<Vec<_>>()
        .join("; ")
}

fn matches_program(coach: &Coach, program: Program) -> bool {
    let role = coach.role.to_ascii_lowercase();
    let xc = coach.sport == Some(Sport::CrossCountry)
        || (coach.sport.is_none()
            && (role.replace([' ', '-'], "").contains("crosscountry")
                || role
                    .split(|ch: char| !ch.is_ascii_alphanumeric())
                    .any(|word| word == "xc")));
    let track = matches!(coach.sport, Some(Sport::Indoor | Sport::Outdoor))
        || (coach.sport.is_none() && role.contains("track"));
    match program {
        Program::Track => track,
        Program::CrossCountry => xc,
        Program::Unspecified => !track && !xc,
    }
}
