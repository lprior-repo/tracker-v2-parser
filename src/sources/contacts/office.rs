use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

use crate::model::{School, Target};

use super::{email, normalized, selector, staff, text};

pub(super) fn enrich(
    document: &Html,
    target: &Target,
    school_name: &str,
    mut schools: Vec<School>,
) -> Result<Vec<School>> {
    let Some(public_email) = parse(document, school_name)? else {
        return Ok(schools);
    };
    if let Some(school) = schools.first_mut() {
        school.public_email = Some(public_email);
    } else {
        schools.push(School {
            id: target
                .context
                .school_id
                .clone()
                .context("office mailbox requires school ID")?,
            name: school_name.to_owned(),
            state: target.context.state.clone(),
            public_email: Some(public_email),
            ..School::default()
        });
    }
    Ok(schools)
}

fn parse(document: &Html, school_name: &str) -> Result<Option<String>> {
    let blocks = selector("p, tr, li, dd, address, .contact-card, .office-contact")?;
    let anchors = selector("a[href^='mailto:' i]")?;
    let label = Regex::new(r"(?i)\b(?:school|main|athletics?|athletic department)\s+office\b")?;
    let excluded =
        Regex::new(r"(?i)\b(?:district|superintendent|teacher|parent|booster|coach|counselor)\b")?;
    let email_rule = email::pattern()?;
    ensure!(
        document.select(&blocks).count() <= 20_000,
        "contacts page exceeds 20000 office blocks"
    );
    let identity = normalized(school_name);
    let headings = selector("title, h1")?;
    let identified = staff::school_local(document, &headings, school_name);
    let candidates = document
        .select(&blocks)
        .filter_map(|block| {
            let content = text(block);
            let office = label.find(&content)?;
            let prefix = normalized(content.get(..office.start())?);
            let named = prefix == identity;
            let generic = matches!(
                prefix.as_str(),
                "" | "contact" | "contactthe" | "email" | "emailthe" | "our" | "the"
            );
            if content.len() > 4096
                || !(named || identified && generic)
                || excluded.is_match(&content)
                || excluded_scope(block, named)
            {
                return None;
            }
            mailbox(block, &content, &anchors, &email_rule)
        })
        .collect::<BTreeSet<_>>();
    Ok(if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    })
}

fn mailbox(
    block: ElementRef<'_>,
    content: &str,
    anchors: &Selector,
    email_rule: &Regex,
) -> Option<String> {
    let linked = block
        .select(anchors)
        .filter(|anchor| !email::hidden(*anchor))
        .filter_map(|anchor| {
            let (_, address) = anchor.value().attr("href")?.split_once(':')?;
            Some(
                address
                    .split_once('?')
                    .map_or(address, |(mailbox, _)| mailbox),
            )
        });
    let visible = content.split_whitespace().filter(|word| word.contains('@'));
    let mailboxes = linked
        .chain(visible)
        .filter_map(|value| email::normalize(value, email_rule))
        .collect::<BTreeSet<_>>();
    if mailboxes.len() == 1 {
        mailboxes.into_iter().next()
    } else {
        None
    }
}

fn excluded_scope(block: ElementRef<'_>, school_named: bool) -> bool {
    email::hidden(block)
        || std::iter::once(block)
            .chain(block.ancestors().filter_map(ElementRef::wrap))
            .any(|parent| {
                matches!(parent.value().name(), "nav" | "header")
                    || (parent.value().name() == "footer" && !school_named)
            })
}
