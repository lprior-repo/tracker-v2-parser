pub mod athleticnet;
pub mod contacts;
pub mod milesplit;

use anyhow::Result;

use crate::model::{PageKind, ParsedPage, Source, Target};

#[derive(Debug)]
pub(crate) struct AccessDenied;

impl std::fmt::Display for AccessDenied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MileSplit access denied, login, subscription, or challenge page; no fallback attempted")
    }
}

impl std::error::Error for AccessDenied {}

pub fn parse(target: &Target, body: &str) -> Result<ParsedPage> {
    if target.kind == PageKind::Contacts {
        return contacts::parse(target, body);
    }
    match target.source {
        Source::AthleticNet => athleticnet::parse(target, body),
        Source::MileSplit => milesplit::parse(target, body),
    }
}

pub fn seeds(source: Source, states: &[String], season: i32) -> Result<Vec<Target>> {
    match source {
        Source::AthleticNet => athleticnet::seeds(states, season),
        Source::MileSplit => milesplit::seeds(states, season),
    }
}
