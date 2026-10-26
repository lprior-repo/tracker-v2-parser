use anyhow::{Result, ensure};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

use crate::model::{Coach, Sport};

use super::{email, selector, text};

struct Rules {
    blocks: Selector,
    anchors: Selector,
    names: Selector,
    titles: Selector,
    programs: Selector,
    scripts: Selector,
    headings: Selector,
    coach: Regex,
    track: Regex,
    cross: Regex,
    excluded: Regex,
    email: Regex,
    halves: Regex,
}

impl Rules {
    fn new() -> Result<Self> {
        Ok(Self {
            blocks: selector(
                "tr, li, p, dd, .staff-card, .staff-member, .staff-profile, .coach-card, .staff-directory-item, [itemtype$='/Person']",
            )?,
            anchors: selector("a[href^='mailto:'], a[href^='MAILTO:']")?,
            names: selector(
                "[itemprop='name'], .staff-name, .coach-name, .name, [headers~='col-fullname']",
            )?,
            titles: selector(
                "[itemprop='jobTitle'], .staff-title, .staff-role, .coach-title, .position, [headers~='col-staff_title']",
            )?,
            programs: selector("[headers~='col-staff_custom_1'], .staff-sport, .coach-sport")?,
            scripts: selector("[headers~='col-staff_email'] script")?,
            headings: selector("h2, h3, h4, h5, h6")?,
            coach: Regex::new(r"(?i)\bcoach(?:es|ing)?\b")?,
            track: Regex::new(r"(?i)\btrack\b|\bfield\s+(?:events?\s+)?coach\b")?,
            cross: Regex::new(r"(?i)\bcross[-\s]?country\b|\bxc\b")?,
            excluded: Regex::new(
                r"(?i)\b(?:parent|booster|retired|former|alumni|memorial|volunteer\s+coordinator)\b",
            )?,
            email: email::pattern()?,
            halves: Regex::new(r#"\b(firstHalf|secondHalf)\s*=\s*(?:"([^"\\]*)"|'([^'\\]*)')"#)?,
        })
    }
}

pub(super) fn parse(document: &Html, school_id: &str) -> Result<Vec<Coach>> {
    let rules = Rules::new()?;
    ensure!(
        document.select(&rules.blocks).count() <= 20_000,
        "contacts page exceeds 20000 staff blocks"
    );
    let rows = document
        .select(&rules.blocks)
        .flat_map(|block| candidates(block, &rules, school_id));
    Ok(rows.fold(Vec::<Coach>::new(), |mut kept, coach| {
        if !kept.iter().any(|existing| {
            existing.email == coach.email
                && existing.role == coach.role
                && existing.sport == coach.sport
        }) {
            kept.push(coach);
        }
        kept
    }))
}

fn candidates(block: ElementRef<'_>, rules: &Rules, school_id: &str) -> Vec<Coach> {
    let content = text(block);
    if content.len() > 4096 || chrome(block) || rules.excluded.is_match(&content) {
        return Vec::new();
    }
    let role = block
        .select(&rules.titles)
        .next()
        .map_or_else(|| content.clone(), text);
    if !rules.coach.is_match(&role) {
        return Vec::new();
    }
    let program = program_text(block, rules, &content);
    let track = rules.track.is_match(&program);
    let cross = rules.cross.is_match(&program);
    if !track && !cross {
        return Vec::new();
    }
    let emails = emails(block, rules, &content);
    let [email] = emails.as_slice() else {
        return Vec::new();
    };
    let name = block.select(&rules.names).next().map(text).filter(|name| {
        !name.is_empty() && name.len() <= 200 && !name.contains('@') && !rules.coach.is_match(name)
    });
    let role = if rules.track.is_match(&role) || rules.cross.is_match(&role) {
        role
    } else {
        format!("{role} — {program}")
    };
    let sports = sports(&program, track, cross);
    sports
        .into_iter()
        .map(|sport| Coach {
            school_id: school_id.to_owned(),
            name: name.clone(),
            role: role.clone(),
            email: email.clone(),
            sport,
        })
        .collect()
}

fn chrome(block: ElementRef<'_>) -> bool {
    block
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|parent| {
            matches!(parent.value().name(), "nav" | "footer" | "header")
                || parent.value().attr("hidden").is_some()
                || parent.value().attr("aria-hidden") == Some("true")
        })
}

fn program_text(block: ElementRef<'_>, rules: &Rules, content: &str) -> String {
    if let Some(program) = block
        .select(&rules.programs)
        .next()
        .map(text)
        .filter(|value| !value.is_empty())
    {
        return program;
    }
    if rules.track.is_match(content) || rules.cross.is_match(content) {
        return content.to_owned();
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
    category
        .or_else(|| preceding_heading(block, rules))
        .map_or(String::new(), core::convert::identity)
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
    let linked = block.select(&rules.anchors).filter_map(|anchor| {
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
        .fold(Vec::new(), |mut kept, value| {
            if !kept.contains(&value) {
                kept.push(value);
            }
            kept
        })
}

fn sanitize(value: &str, rules: &Rules) -> Option<String> {
    let value = email::normalize(value, &rules.email)?;
    let (local, _) = value.split_once('@')?;
    if [
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
    .contains(&local.to_ascii_lowercase().as_str())
    {
        return None;
    }
    Some(value)
}

fn sidearm_email(block: ElementRef<'_>, rules: &Rules) -> Option<String> {
    let script = block.select(&rules.scripts).next()?;
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
