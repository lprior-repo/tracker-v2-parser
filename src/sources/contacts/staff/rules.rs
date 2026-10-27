use anyhow::Result;
use regex::Regex;
use scraper::Selector;

use super::super::{email, selector};

pub(super) struct Rules {
    pub(super) blocks: Selector,
    pub(super) cards: Selector,
    pub(super) anchors: Selector,
    pub(super) names: Selector,
    pub(super) titles: Selector,
    pub(super) programs: Selector,
    pub(super) scripts: Selector,
    pub(super) school_headings: Selector,
    pub(super) affiliations: Selector,
    pub(super) headings: Selector,
    pub(super) obfuscated: Selector,
    pub(super) coach: Regex,
    pub(super) track: Regex,
    pub(super) cross: Regex,
    pub(super) other_sport: Regex,
    pub(super) excluded: Regex,
    pub(super) email: Regex,
    pub(super) halves: Regex,
}

impl Rules {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            blocks: selector(
                "tr, li, p, dd, .staff-card, .staff-member, .staff-profile, .coach-card, .staff-directory-item, .fsConstituentItem, [itemtype$='/Person']",
            )?,
            cards: selector(
                "tr, .staff-card, .staff-member, .staff-profile, .coach-card, .staff-directory-item, .fsConstituentItem, [itemtype$='/Person']",
            )?,
            anchors: selector("a[href^='mailto:'], a[href^='MAILTO:']")?,
            names: selector(
                "[itemprop='name'], .staff-name, .coach-name, .name, .fsFullName, [headers~='col-fullname'], #tblCoachList .formLabelsText",
            )?,
            titles: selector(
                "[itemprop='jobTitle'], .staff-title, .staff-role, .coach-title, .position, .fsTitles, [headers~='col-staff_title'], #tblCoachList td:nth-child(4)",
            )?,
            programs: selector(
                "[headers~='col-staff_custom_1'], .staff-sport, .coach-sport, #tblCoachList td:nth-child(2)",
            )?,
            scripts: selector("[headers~='col-staff_email'] script")?,
            headings: selector("h2, h3, h4, h5, h6")?,
            school_headings: selector("title, h1")?,
            affiliations: selector("[itemprop='affiliation'], .staff-school, .coach-school")?,
            obfuscated: selector("[data-cfemail], a[href*='/cdn-cgi/l/email-protection#']")?,
            coach: Regex::new(r"(?i)\bcoach(?:es|ing)?\b")?,
            track: Regex::new(r"(?i)\btrack\b|\bfield\s+(?:events?\s+)?coach\b")?,
            cross: Regex::new(r"(?i)\bcross[-\s]?country\b|\bxc\b")?,
            other_sport: Regex::new(
                r"(?i)\b(?:basketball|football|baseball|softball|volleyball|soccer|wrestling|golf|tennis|swimming|diving|lacrosse|hockey|cheerleading|water\s+polo)\b",
            )?,
            excluded: Regex::new(
                r"(?i)\b(?:parent|booster|retired|former|alumni|memorial|volunteer\s+coordinator)\b",
            )?,
            email: email::pattern()?,
            halves: Regex::new(r#"\b(firstHalf|secondHalf)\s*=\s*(?:"([^"\\]*)"|'([^'\\]*)')"#)?,
        })
    }
}
