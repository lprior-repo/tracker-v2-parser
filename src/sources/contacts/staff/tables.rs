use anyhow::{Result, ensure};
use scraper::{ElementRef, Html};

use crate::model::Coach;
use crate::sources::contacts::{normalized, selector};

use super::{Rules, chrome, emails, scope, sports, text};

struct Columns {
    program: usize,
    name: usize,
    mailbox: usize,
    role: &'static str,
}

pub(super) fn parse(
    document: &Html,
    rules: &Rules,
    school_id: &str,
    school_name: &str,
    page_local: bool,
) -> Result<Vec<Coach>> {
    let tables = selector("table")?;
    let rows = selector("tr")?;
    let labels = selector("[id]")?;
    ensure!(
        document.select(&tables).count() <= 128,
        "contacts page exceeds 128 tables"
    );
    ensure!(
        document.select(&labels).count() <= 20_000,
        "contacts page exceeds 20000 labeled elements"
    );
    Ok(document
        .select(&tables)
        .filter(|table| !chrome(*table))
        .flat_map(|table| {
            let own_rows = || {
                table.select(&rows).filter(|row| {
                    row.ancestors()
                        .filter_map(ElementRef::wrap)
                        .find(|ancestor| ancestor.value().name() == "table")
                        .is_some_and(|parent| parent.id() == table.id())
                })
            };
            let headers = own_rows()
                .filter(|row| cells(*row).iter().any(|cell| cell.value().name() == "th"))
                .collect::<Vec<_>>();
            let columns = match headers.as_slice() {
                [] => section_role(document, table, &labels).map(|role| Columns {
                    program: 0,
                    name: 1,
                    mailbox: 2,
                    role,
                }),
                [header] => header_columns(*header),
                _ => None,
            };
            own_rows()
                .filter(|row| !chrome(*row))
                .flat_map(|row| {
                    columns.as_ref().map_or_else(Vec::new, |columns| {
                        row_coaches(row, columns, rules, school_id, school_name, page_local)
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

fn cells(row: ElementRef<'_>) -> Vec<ElementRef<'_>> {
    row.children()
        .filter_map(ElementRef::wrap)
        .filter(|cell| matches!(cell.value().name(), "td" | "th"))
        .take(4)
        .collect()
}

fn header_columns(row: ElementRef<'_>) -> Option<Columns> {
    let headers = cells(row);
    if headers.len() != 3 || headers.iter().any(|cell| chrome(*cell) || spanned(*cell)) {
        return None;
    }
    let headers = headers
        .iter()
        .map(|cell| normalized(&text(*cell)))
        .collect::<Vec<_>>();
    let program = unique_column(&headers, &["activity", "sport", "program"])?;
    let name = unique_column(
        &headers,
        &["coach", "coachadvisor", "coachname", "headcoach"],
    )?;
    let mailbox = unique_column(&headers, &["contact", "email", "coachemail"])?;
    Some(Columns {
        program,
        name,
        mailbox,
        role: if headers.get(name).is_some_and(|name| name == "headcoach") {
            "Head Coach"
        } else {
            "Coach"
        },
    })
}

fn unique_column(headers: &[String], accepted: &[&str]) -> Option<usize> {
    let matches = headers
        .iter()
        .enumerate()
        .filter_map(|(index, header)| accepted.contains(&header.as_str()).then_some(index))
        .take(2)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Some(*index),
        _ => None,
    }
}

fn section_role(
    document: &Html,
    table: ElementRef<'_>,
    labels: &scraper::Selector,
) -> Option<&'static str> {
    let region = table
        .ancestors()
        .filter_map(ElementRef::wrap)
        .find(|ancestor| ancestor.value().attr("role") == Some("region"))?;
    let label = region.value().attr("aria-labelledby")?;
    if label.split_whitespace().count() != 1 {
        return None;
    }
    let matches = document
        .select(labels)
        .filter(|element| element.value().id() == Some(label))
        .take(2)
        .collect::<Vec<_>>();
    let [heading] = matches.as_slice() else {
        return None;
    };
    if chrome(*heading) {
        return None;
    }
    match normalized(&text(*heading)).as_str() {
        "headcoaches" | "fallheadcoaches" | "winterheadcoaches" | "springheadcoaches" => {
            Some("Head Coach")
        }
        "coaches" | "fallcoaches" | "wintercoaches" | "springcoaches" => Some("Coach"),
        _ => None,
    }
}

fn row_coaches(
    row: ElementRef<'_>,
    columns: &Columns,
    rules: &Rules,
    school_id: &str,
    school_name: &str,
    page_local: bool,
) -> Vec<Coach> {
    let cells = cells(row);
    if cells.len() != 3
        || cells
            .iter()
            .any(|cell| cell.value().name() != "td" || chrome(*cell) || spanned(*cell))
    {
        return Vec::new();
    }
    let Some(program_cell) = cells.get(columns.program) else {
        return Vec::new();
    };
    let Some(name_cell) = cells.get(columns.name) else {
        return Vec::new();
    };
    let Some(mailbox_cell) = cells.get(columns.mailbox) else {
        return Vec::new();
    };
    let program = text(*program_cell);
    let name = text(*name_cell);
    let track = rules.track.is_match(&program);
    let cross = rules.cross.is_match(&program);
    if (!track && !cross)
        || program.len() > 200
        || rules.other_sport.is_match(&program)
        || rules.excluded.is_match(&program)
        || rules.excluded.is_match(&name)
        || rules.skiing_only(&program)
        || !person_name(&name)
    {
        return Vec::new();
    }
    let mailboxes = emails(*mailbox_cell, rules, &text(*mailbox_cell));
    let mailbox = match mailboxes.as_slice() {
        [mailbox] => Some(mailbox.clone()),
        _ => None,
    };
    let review_notes = scope::review_notes(row, rules, school_name, page_local);
    sports(&program, track, cross)
        .into_iter()
        .map(|sport| Coach {
            school_id: school_id.to_owned(),
            name: Some(name.clone()),
            role: format!("{} — {program}", columns.role),
            email: mailbox.clone(),
            sport,
            review_notes: review_notes.clone(),
        })
        .collect()
}

fn spanned(cell: ElementRef<'_>) -> bool {
    ["colspan", "rowspan"].iter().any(|attribute| {
        cell.value()
            .attr(attribute)
            .is_some_and(|value| value.trim() != "1")
    })
}

fn person_name(name: &str) -> bool {
    (2..=8).contains(&name.split_whitespace().count())
        && name.len() <= 200
        && name
            .chars()
            .all(|c| c.is_alphabetic() || c.is_whitespace() || matches!(c, '-' | '\'' | '’' | '.'))
        && ![
            "to be determined",
            "not available",
            "not assigned",
            "head coach",
            "assistant coach",
        ]
        .iter()
        .any(|placeholder| name.eq_ignore_ascii_case(placeholder))
        && !name.to_ascii_lowercase().contains(" and ")
}
