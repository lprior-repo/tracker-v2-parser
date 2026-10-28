use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, ensure};

use crate::{
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Target},
    sources::wiaa,
};

use super::{
    Report,
    evidence::{self, Association},
    proof::{self, Program},
    schedule::{self, Confirmation},
};

pub(super) struct Prepared {
    pub source: Source,
    pub school_id: String,
    pub school_name: String,
    pub target: Target,
    pub capture: Capture,
    pub page: ParsedPage,
}

pub(super) fn prepare(path: &Path) -> Result<(Vec<Prepared>, Report)> {
    let (manifest, manifest_sha256) = evidence::manifest(path)?;
    let mut total = 0;
    let mut kinds = BTreeSet::new();
    let programs = manifest
        .program_reports
        .iter()
        .try_fold(Vec::new(), |mut programs, item| {
            let body = evidence::capture(item, &mut total)?;
            let (kind, parsed) = proof::report(item, &body)?;
            ensure!(
                kinds.insert(kind),
                "duplicate association report sport/gender"
            );
            programs.extend(parsed);
            Ok::<_, anyhow::Error>(programs)
        })?;
    let mut report = Report {
        manifest_sha256,
        captures_validated: 4,
        ..Report::default()
    };
    let mut identities = BTreeSet::new();
    let pages = manifest.associations.iter().map(|association| {
        ensure!(identities.insert((association.provider_source, association.provider_school_id.clone())), "duplicate provider co-op mapping");
        proof::mapping(association)?;
        let body = evidence::capture(&association.host_capture, &mut total)?;
        report.captures_validated = report.captures_validated.saturating_add(1);
        ensure!(association.current_schedules.len() <= 4, "co-op schedule evidence exceeds four programs");
        let schedules = association.current_schedules.iter().map(|capture| {
            let body = evidence::capture(capture, &mut total)?;
            report.captures_validated = report.captures_validated.saturating_add(1);
            schedule::parse(association, capture, &body)
        }).collect::<Result<Vec<_>>>()?;
        let matching = programs.iter().filter(|program| {
            proof::normalized(&program.cooperative) == proof::normalized(&association.published_cooperative_name)
        }).collect::<Vec<_>>();
        ensure!(!matching.is_empty(), "co-op {} is absent from the four current reports", association.provider_school_name);
        ensure!(matching.iter().all(|program| proof::normalized(&program.host) == proof::normalized(&association.designated_contact_school)), "co-op report disagrees with designated host");
        let mut program_keys = BTreeSet::new();
        ensure!(matching.iter().all(|program| program_keys.insert(program.role_suffix())), "co-op has ambiguous duplicate program proof");
        let mut schedule_keys = BTreeSet::new();
        ensure!(schedules.iter().all(|schedule| schedule_keys.insert((&schedule.sport, &schedule.gender)) && matching.iter().any(|program| schedule.matches(program))), "current schedule does not match a unique report-endorsed program");
        let approved = matching.into_iter().filter(|program| {
            if program.confirmed() || schedules.iter().any(|schedule| schedule.matches(program)) { true } else {
                report.unconfirmed_programs.push(format!("{}: {}; published Expire Yr {} and Deadline {}; current validity interpretation unconfirmed", association.provider_school_name, program.role_suffix(), program.expiration, program.deadline));
                false
            }
        }).collect::<Vec<_>>();
        report.programs_imported = report.programs_imported.saturating_add(approved.len());
        let prepared = page(association, &body, &approved, &schedules, &report.manifest_sha256)?;
        report.coaches_imported = report.coaches_imported.saturating_add(prepared.page.coaches.len());
        Ok(prepared)
    }).collect::<Result<Vec<_>>>()?;
    ensure!(
        pages.iter().all(|page| !page.page.coaches.is_empty()),
        "a mapped co-op has no endorsed public coach claims"
    );
    report.associations_imported = pages.len();
    report.unconfirmed_programs.sort();
    Ok((pages, report))
}

fn page(
    association: &Association,
    body: &str,
    programs: &[&Program],
    schedules: &[Confirmation],
    manifest_sha: &str,
) -> Result<Prepared> {
    let mut target = Target::get(
        association.provider_source,
        PageKind::Contacts,
        association.host_capture.url.clone(),
        PageContext {
            school_id: Some(association.provider_school_id.clone()),
            school_name: Some(association.designated_contact_school.clone()),
            state: Some("WI".into()),
            season: Some(2026),
            subject_id: Some("contacts-depth:2".into()),
            ..PageContext::default()
        },
    );
    let parsed = wiaa::parse(&target, body).with_context(|| {
        format!(
            "validating designated co-op host {}",
            association.designated_contact_school
        )
    })?;
    ensure!(
        schedules
            .iter()
            .all(|schedule| parsed.coaches.iter().any(|coach| {
                coach.role == format!("Head Coach — {} {}", schedule.gender, schedule.sport)
                    && coach.name.as_deref().is_some_and(|name| {
                        proof::normalized(name) == proof::normalized(&schedule.coach)
                    })
            })),
        "current schedule head coach disagrees with designated host coach table"
    );
    let coaches = parsed
        .coaches
        .into_iter()
        .filter_map(|mut coach| {
            let matching = programs.iter().find(|program| {
                coach
                    .role
                    .ends_with(&format!(" — {}", program.role_suffix()))
            })?;
            if !matching.confirmed() {
                let confirmation = schedules.iter().find(|schedule| schedule.matches(matching))?;
                if coach.name.as_deref().is_none_or(|name| proof::normalized(name) != proof::normalized(&confirmation.coach)) {
                    return None;
                }
                coach.review_notes.push(confirmation.attribution.clone());
            }
            coach.school_id.clone_from(&association.provider_school_id);
            coach.review_notes.push(matching.attribution());
            coach.review_notes.push(format!(
                "Designated host {} SHA-256 {} captured {}; manifest SHA-256 {}; cooperative source identity preserved",
                association.host_capture.url,
                association.host_capture.sha256,
                association.host_capture.captured_utc,
                manifest_sha
            ));
            Some(coach)
        })
        .collect();
    target.context.school_name = Some(association.provider_school_name.clone());
    Ok(Prepared {
        source: association.provider_source,
        school_id: association.provider_school_id.clone(),
        school_name: association.provider_school_name.clone(),
        target,
        capture: Capture {
            url: association.host_capture.url.clone(),
            status: 200,
            content_type: "text/html".into(),
            body: body.as_bytes().to_vec(),
        },
        page: ParsedPage {
            coaches,
            notes: vec![format!(
                "Cooperative association manifest SHA-256 {manifest_sha}"
            )],
            ..ParsedPage::default()
        },
    })
}
