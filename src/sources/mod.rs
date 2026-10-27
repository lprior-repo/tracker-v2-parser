pub mod athleticnet;
pub mod contacts;
pub mod milesplit;

use anyhow::Result;

use crate::{
    model::{PageKind, ParsedPage, School, Source, Target},
    transport::validate_target,
};

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
    let page = match target.source {
        Source::AthleticNet => athleticnet::parse(target, body),
        Source::MileSplit => milesplit::parse(target, body),
    }?;
    let (next, notes) =
        page.schools
            .iter()
            .fold((page.next, page.notes), |(mut next, mut notes), school| {
                match school_contacts_for_target(target, school) {
                    Ok(targets) => next.extend(targets),
                    Err(error) => {
                        notes.push(format!("School {} contact discovery: {error:#}", school.id));
                    }
                }
                (next, notes)
            });
    Ok(ParsedPage {
        next,
        notes,
        ..page
    })
}

fn school_contacts_for_target(target: &Target, school: &School) -> Result<Vec<Target>> {
    if school
        .website
        .as_deref()
        .is_none_or(|site| site.trim().is_empty())
    {
        return Ok(Vec::new());
    }
    let season = target.context.season.ok_or_else(|| {
        anyhow::anyhow!("contact discovery deferred: athletics target has no season")
    })?;
    school_contacts(target.source, school, season)
}

pub(crate) fn school_contacts(source: Source, school: &School, season: i32) -> Result<Vec<Target>> {
    let targets = contacts::targets(source, school, season)?;
    targets.iter().try_for_each(validate_target)?;
    Ok(targets)
}

pub fn seeds(source: Source, states: &[String], season: i32) -> Result<Vec<Target>> {
    match source {
        Source::AthleticNet => athleticnet::seeds(states, season),
        Source::MileSplit => milesplit::seeds(states, season),
    }
}
