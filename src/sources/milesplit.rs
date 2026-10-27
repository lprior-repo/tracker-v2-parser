mod directory;
mod html;
mod manual;
mod meets;
mod owned;
mod profile;
mod raw;
mod targets;
mod team;
#[cfg(test)]
mod tests;

use anyhow::{Result, bail, ensure};
use scraper::Html;

use crate::model::{PageKind, ParsedPage, Source, Target};

pub use targets::{athlete_target, meet_target, seeds, team_target};

pub fn parse(target: &Target, body: &str) -> Result<ParsedPage> {
    ensure!(target.source == Source::MileSplit, "not a MileSplit target");
    ensure!(
        body.len() <= 16 * 1024 * 1024,
        "MileSplit capture exceeds 16 MiB"
    );
    if target.kind == PageKind::Results && !target.context.result_set_ids.is_empty() {
        return owned::parse(target, body);
    }
    targets::public_url(&target.url)?;
    let document = Html::parse_document(body);
    html::access(&document)?;
    match target.kind {
        PageKind::Directory if target.url.contains("/teams") => {
            directory::schools(target, &document)
        }
        PageKind::Directory => directory::meets(target, &document),
        PageKind::Team | PageKind::Roster => team::parse(target, &document),
        PageKind::Athlete => profile::parse(target, &document),
        PageKind::Meet => meets::parse(target, body, &document),
        PageKind::Results => meets::parse_result(target, &document),
        PageKind::Contacts => bail!("MileSplit contacts page is not a supported result surface"),
    }
}
