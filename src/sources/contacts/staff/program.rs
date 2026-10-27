use scraper::ElementRef;

use super::{Rules, chrome, text};

pub(super) fn program_text(
    block: ElementRef<'_>,
    rules: &Rules,
    role: &str,
) -> (String, Option<String>) {
    let running_role = rules.track.is_match(role) || rules.cross.is_match(role);
    if !running_role && rules.other_sport.is_match(role) {
        return (role.to_owned(), None);
    }
    if let Some(program) = block
        .select(&rules.programs)
        .find(|program| !chrome(*program))
        .map(text)
        .filter(|value| !value.is_empty())
    {
        return resolve_program(role, program, rules, running_role);
    }
    if running_role {
        return (role.to_owned(), None);
    }
    let category = block.value().attr("data-category-id").and_then(|category| {
        block
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|parent| parent.value().name() == "table")
            .and_then(|table| {
                table.select(&rules.blocks).find(|row| {
                    row.value()
                        .classes()
                        .any(|class| class == "sidearm-staff-category")
                        && row.value().attr("data-category-id") == Some(category)
                })
            })
            .map(text)
    });
    (
        category
            .or_else(|| preceding_heading(block, rules))
            .map_or(String::new(), core::convert::identity),
        None,
    )
}

fn resolve_program(
    role: &str,
    program: String,
    rules: &Rules,
    running_role: bool,
) -> (String, Option<String>) {
    if !running_role {
        return (program, None);
    }
    let role_track = rules.track.is_match(role);
    let program_track = rules.track.is_match(&program);
    let role_lower = role.to_ascii_lowercase();
    let program_lower = program.to_ascii_lowercase();
    let specific_track = role_lower.contains("indoor") || role_lower.contains("outdoor");
    let unsupported_season = specific_track
        && ["indoor", "outdoor"]
            .iter()
            .any(|season| program_lower.contains(season) && !role_lower.contains(season));
    let unsupported = (program_track && !role_track)
        || (rules.cross.is_match(&program) && !rules.cross.is_match(role))
        || unsupported_season
        || (!program_track && !rules.cross.is_match(&program));
    if unsupported {
        return (
            role.to_owned(),
            Some(format!(
                "Program context requires review: published program \"{program}\" does not establish an additional duty beyond explicit role \"{role}\"; only the explicit role was retained."
            )),
        );
    }
    if role_track && !specific_track && program_track {
        return (format!("{role} — {program}"), None);
    }
    (role.to_owned(), None)
}

fn preceding_heading(block: ElementRef<'_>, rules: &Rules) -> Option<String> {
    block
        .prev_siblings()
        .take(24)
        .filter_map(ElementRef::wrap)
        .find_map(|previous| {
            if matches!(previous.value().name(), "h2" | "h3" | "h4" | "h5" | "h6") {
                Some(text(previous))
            } else if previous.select(&rules.headings).next().is_some() {
                Some(String::new())
            } else {
                None
            }
        })
}
