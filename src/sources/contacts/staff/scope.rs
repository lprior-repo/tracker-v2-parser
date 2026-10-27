use scraper::{ElementRef, Html};

use super::{Rules, chrome, text};
use crate::sources::contacts::normalized;

pub(super) fn page_local(document: &Html, rules: &Rules, school_name: &str) -> bool {
    let identity = normalized(school_name);
    let (matched, unambiguous) = document
        .select(&rules.school_headings)
        .filter(|heading| !chrome(*heading))
        .fold((false, true), |(matched, unambiguous), heading| {
            let heading = normalized(&text(heading));
            let matching = heading
                .strip_prefix(identity.as_str())
                .is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "" | "athletics"
                            | "staff"
                            | "staffdirectory"
                            | "coaches"
                            | "coachesdirectory"
                    )
                });
            let neutral = matches!(
                heading.as_str(),
                "" | "athletics" | "staff" | "staffdirectory" | "coaches" | "coachesdirectory"
            );
            (matched || matching, unambiguous && (matching || neutral))
        });
    matched && unambiguous
}

pub(super) fn review_notes(
    block: ElementRef<'_>,
    rules: &Rules,
    school_name: &str,
    page_local: bool,
) -> Vec<String> {
    let identity = normalized(school_name);
    let affiliations = block
        .select(&rules.affiliations)
        .filter(|affiliation| !chrome(*affiliation))
        .map(text)
        .filter(|affiliation| !affiliation.is_empty());
    let (published, matches) = affiliations.fold((false, true), |(_, matches), affiliation| {
        (true, matches && normalized(&affiliation) == identity)
    });
    let affiliated = if published { matches } else { page_local };
    [
        (!affiliated).then_some("School affiliation unconfirmed: no unambiguous matching school-local page or staff-card context was established; retained under the requested school for review."),
        Some("Current tenure unconfirmed: no explicit current appointment period was extracted; capture date alone does not establish current tenure."),
    ]
    .into_iter()
    .flatten()
    .map(str::to_owned)
    .collect()
}
