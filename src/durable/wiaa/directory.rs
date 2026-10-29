use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::durable::operations::{Artifact, Workspace};
use crate::{
    config::Config,
    database::Database,
    model::{PageKind, ParsedPage, Target},
    sources::wiaa::{self, DirectorySchool},
};

const ENTRIES: &str = "WIAA captured directory entries: ";

pub(in crate::durable) fn is_index(target: &Target) -> bool {
    target.kind == PageKind::Contacts
        && target.context.subject_id.as_deref() == Some("wiaa-directory-index")
        && wiaa::is_public_directory_url(&target.url)
        && !wiaa::is_school_url(&target.url)
}

pub(in crate::durable) fn parse_index(target: &Target, body: &str) -> Result<ParsedPage> {
    ensure!(is_index(target), "invalid durable WIAA index target");
    crate::sources::contacts::check_public_access(body, &target.url)?;
    if target.url == "https://schools.wiaawi.org/Directory/School/List" {
        ensure!(
            target
                .method
                .as_deref()
                .is_none_or(|method| method == "GET"),
            "WIAA root must be GET"
        );
        let next = wiaa::listing_requests(body)?
            .into_iter()
            .map(|url| {
                let mut letter = Target::get(
                    target.source,
                    PageKind::Contacts,
                    url,
                    target.context.clone(),
                );
                letter.method = Some("POST".into());
                letter
            })
            .collect();
        Ok(ParsedPage {
            next,
            ..Default::default()
        })
    } else {
        ensure!(
            target.method.as_deref() == Some("POST") && target.body.is_none(),
            "WIAA letter must be the published empty POST"
        );
        let rows = wiaa::parse_listing(body)?;
        Ok(ParsedPage {
            notes: vec![format!("{ENTRIES}{}", serde_json::to_string(&rows)?)],
            ..Default::default()
        })
    }
}

pub(super) fn entries(pages: &[ParsedPage]) -> Result<Vec<DirectorySchool>> {
    let entries = pages
        .iter()
        .flat_map(|page| page.notes.iter())
        .filter_map(|note| note.strip_prefix(ENTRIES))
        .try_fold(BTreeMap::new(), |mut all, note| -> Result<_> {
            let rows: Vec<DirectorySchool> = serde_json::from_str(note)?;
            ensure!(rows.len() <= 1000, "WIAA letter entry limit");
            rows.into_iter().try_for_each(|row| -> Result<()> {
                if let Some(previous) = all.insert(row.organization_id.clone(), row.clone()) {
                    ensure!(
                        previous == row,
                        "WIAA directory contains conflicting organization identities"
                    );
                }
                Ok(())
            })?;
            ensure!(all.len() <= 10_000, "WIAA directory entry limit");
            Ok(all)
        })?;
    Ok(entries.into_values().collect())
}

#[derive(Serialize, Deserialize)]
pub(super) struct FrozenPlan {
    pub targets: Artifact,
    pub report: Value,
    input_sha256: String,
}

pub(super) async fn frozen_plan(
    workspace: &Workspace,
    database: &Database,
    config: &Config,
    entries: &[DirectorySchool],
) -> Result<FrozenPlan> {
    let input_sha256 = crate::durable::model::digest(&(workspace.binding(), config, entries))?;
    let existing = workspace.path("wiaa-frozen-plan.json")?;
    if existing.try_exists()? {
        let plan: FrozenPlan =
            serde_json::from_slice(&workspace.read(&workspace.inspect("wiaa-frozen-plan.json")?)?)?;
        ensure!(
            plan.input_sha256 == input_sha256,
            "WIAA mapping plan input changed"
        );
        workspace.read(&plan.targets)?;
        return Ok(plan);
    }
    let schools = if workspace.path("wiaa-provider-schools.json")?.try_exists()? {
        serde_json::from_slice(&workspace.read(&workspace.inspect("wiaa-provider-schools.json")?)?)?
    } else {
        let schools = crate::wiaa_enrichment::schools(database, &config.enabled_sources()).await?;
        workspace.store("wiaa-provider-schools.json", &serde_json::to_vec(&schools)?)?;
        schools
    };
    let (targets, report) = crate::wiaa_enrichment::prepare(&schools, entries, config.season)?;
    let targets = workspace.store("wiaa-targets.json", &serde_json::to_vec(&targets)?)?;
    let plan = FrozenPlan {
        targets,
        report: serde_json::to_value(report)?,
        input_sha256,
    };
    workspace.store("wiaa-frozen-plan.json", &serde_json::to_vec(&plan)?)?;
    ensure!(
        plan.report.as_object().is_some(),
        "invalid WIAA mapping report"
    );
    Ok(plan)
}
