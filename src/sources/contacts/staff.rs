mod program;
mod rules;
mod scope;

use anyhow::{Result, ensure};
use scraper::{ElementRef, Html};

use crate::model::{Coach, Sport};

use super::{email, staff_text as text};
use program::program_text;
use rules::Rules;
pub(super) use scope::school_local;

pub(super) fn parse(document: &Html, school_id: &str, school_name: &str) -> Result<Vec<Coach>> {
    let rules = Rules::new()?;
    let page_local = scope::page_local(document, &rules, school_name);
    ensure!(
        document.select(&rules.blocks).count() <= 20_000,
        "contacts page exceeds 20000 staff blocks"
    );
    let rows = document
        .select(&rules.blocks)
        .flat_map(|block| candidates(block, &rules, school_id, school_name, page_local));
    Ok(rows.fold(Vec::<Coach>::new(), |mut kept, coach| {
        if let Some(existing) = kept.iter_mut().find(|existing| {
            existing.email == coach.email
                && existing.name == coach.name
                && existing.role == coach.role
                && existing.sport == coach.sport
        }) {
            existing.review_notes.extend(coach.review_notes);
            existing.review_notes.sort_unstable();
            existing.review_notes.dedup();
        } else {
            kept.push(coach);
        }
        kept
    }))
}

fn candidates(
    block: ElementRef<'_>,
    rules: &Rules,
    school_id: &str,
    school_name: &str,
    page_local: bool,
) -> Vec<Coach> {
    let content = text(block);
    if content.len() > 4096 || chrome(block) || nested_card(block, rules) {
        return Vec::new();
    }
    let role = published_role(block, rules, &content);
    if !rules.coach.is_match(&role) || rules.excluded.is_match(&role) {
        return Vec::new();
    }
    let (program, program_review) = program_text(block, rules, &role);
    let track = rules.track.is_match(&program);
    let cross = rules.cross.is_match(&program);
    if !track && !cross {
        return Vec::new();
    }
    let names = published_names(block, rules);
    if names.len() > 1 {
        return Vec::new();
    }
    let name = names.into_iter().next();
    let emails = emails(block, rules, &content);
    let email = if emails.len() == 1 {
        emails.into_iter().next()
    } else {
        None
    };
    if name.is_none() && email.is_none() {
        return Vec::new();
    }
    let role = if rules.track.is_match(&role) || rules.cross.is_match(&role) {
        role
    } else {
        format!("{role} — {program}")
    };
    let mut review_notes = scope::review_notes(block, rules, school_name, page_local);
    review_notes.extend(program_review);
    let sports = sports(&program, track, cross);
    sports
        .into_iter()
        .map(|sport| Coach {
            school_id: school_id.to_owned(),
            name: name.clone(),
            role: role.clone(),
            email: email.clone(),
            sport,
            review_notes: review_notes.clone(),
        })
        .collect()
}

fn nested_card(block: ElementRef<'_>, rules: &Rules) -> bool {
    block
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|parent| rules.cards.matches(&parent))
}

fn published_role(block: ElementRef<'_>, rules: &Rules, content: &str) -> String {
    block
        .select(&rules.titles)
        .find(|title| !chrome(*title))
        .map_or_else(
            || content.to_owned(),
            |title| {
                let role = text(title);
                if title.value().classes().any(|class| class == "fsTitles") {
                    let stripped = role
                        .strip_prefix("Titles:")
                        .map(|value| value.trim().to_owned());
                    stripped.map_or(role, core::convert::identity)
                } else {
                    role
                }
            },
        )
}

fn published_names(block: ElementRef<'_>, rules: &Rules) -> Vec<String> {
    block
        .select(&rules.names)
        .filter(|name| !chrome(*name))
        .map(text)
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 200
                && !name.contains('@')
                && !rules.coach.is_match(name)
        })
        .fold(Vec::new(), |mut names, name| {
            if !names.contains(&name) {
                names.push(name);
            }
            names
        })
}

fn chrome(block: ElementRef<'_>) -> bool {
    email::hidden_publication(block)
        || std::iter::once(block)
            .chain(block.ancestors().filter_map(ElementRef::wrap))
            .any(|parent| matches!(parent.value().name(), "nav" | "footer" | "header"))
}

fn sports(program: &str, track: bool, cross: bool) -> Vec<Option<Sport>> {
    let lower = program.to_ascii_lowercase();
    let indoor = lower.contains("indoor");
    let outdoor = lower.contains("outdoor");
    [
        cross.then_some(Some(Sport::CrossCountry)),
        (track && indoor).then_some(Some(Sport::Indoor)),
        (track && outdoor).then_some(Some(Sport::Outdoor)),
        (track && !indoor && !outdoor).then_some(None),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn emails(block: ElementRef<'_>, rules: &Rules, content: &str) -> Vec<String> {
    let linked = block
        .select(&rules.anchors)
        .filter(|anchor| !chrome(*anchor))
        .filter_map(|anchor| {
            let href = anchor.value().attr("href")?;
            let (_, mailbox) = href.split_once(':')?;
            Some(
                mailbox
                    .split_once('?')
                    .map_or(mailbox, |(address, _)| address),
            )
        });
    let visible = content.split_whitespace().filter(|word| word.contains('@'));
    let split_email = sidearm_email(block, rules);
    linked
        .chain(visible)
        .chain(split_email.as_deref())
        .filter_map(|value| sanitize(value, rules))
        .chain(obfuscated_emails(block, rules))
        .fold(Vec::new(), |mut kept, value| {
            if !kept.contains(&value) {
                kept.push(value);
            }
            kept
        })
}

fn obfuscated_emails<'a>(
    block: ElementRef<'a>,
    rules: &'a Rules,
) -> impl Iterator<Item = String> + 'a {
    std::iter::once(block)
        .chain(block.select(&rules.obfuscated))
        .filter(|element| !chrome(*element))
        .flat_map(|element| {
            let fragment = element.value().attr("href").and_then(|href| {
                let (path, token) = href.split_once('#')?;
                path.ends_with("/cdn-cgi/l/email-protection")
                    .then_some(token)
            });
            [element.value().attr("data-cfemail"), fragment]
                .into_iter()
                .flatten()
        })
        .filter_map(|token| email::decode_cfemail(token, &rules.email))
        .filter(|value| coaching_mailbox(value))
}

fn sanitize(value: &str, rules: &Rules) -> Option<String> {
    let value = email::normalize(value, &rules.email)?;
    coaching_mailbox(&value).then_some(value)
}

fn coaching_mailbox(value: &str) -> bool {
    value.split_once('@').is_some_and(|(local, _)| {
        ![
            "info",
            "office",
            "admin",
            "webmaster",
            "attendance",
            "noreply",
            "no-reply",
            "parents",
            "boosters",
        ]
        .contains(&local)
    })
}

fn sidearm_email(block: ElementRef<'_>, rules: &Rules) -> Option<String> {
    let script = block
        .select(&rules.scripts)
        .find(|script| !chrome(*script))?;
    let body = script.text().collect::<String>();
    let half = |name: &str| {
        rules
            .halves
            .captures_iter(&body)
            .find(|capture| capture.get(1).is_some_and(|value| value.as_str() == name))
            .and_then(|capture| capture.get(2).or_else(|| capture.get(3)))
            .map(|value| value.as_str().trim())
    };
    Some(format!("{}@{}", half("firstHalf")?, half("secondHalf")?))
}
