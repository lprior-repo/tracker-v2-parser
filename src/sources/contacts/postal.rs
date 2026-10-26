use anyhow::{Context, Result, ensure};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use crate::model::{School, Target};

use super::{normalized, public_url, selector, text};

pub(super) fn parse(document: &Html, target: &Target, name: &str) -> Result<Vec<School>> {
    let scripts = selector("script[type='application/ld+json']")?;
    let values = document
        .select(&scripts)
        .map(|script| {
            serde_json::from_str::<Value>(&script.inner_html())
                .context("malformed school-page JSON-LD")
        })
        .collect::<Result<Vec<_>>>()?;
    let candidates = values
        .iter()
        .flat_map(nodes)
        .filter_map(|node| json_school(node, target, name));
    let mut schools = candidates.collect::<Vec<_>>();
    schools.extend(html_schools(document, target, name)?);
    schools.dedup_by(|left, right| {
        left.address == right.address
            && left.postal_code == right.postal_code
            && left.city == right.city
    });
    ensure!(
        schools.len() <= 1,
        "official page publishes conflicting school-matched postal addresses"
    );
    Ok(schools)
}

fn nodes(root: &Value) -> impl Iterator<Item = &Value> {
    root.as_array()
        .map_or_else(|| std::slice::from_ref(root), Vec::as_slice)
        .iter()
        .flat_map(|value| {
            value
                .get("@graph")
                .and_then(Value::as_array)
                .map_or_else(|| std::slice::from_ref(value), Vec::as_slice)
                .iter()
        })
}

fn json_school(node: &Value, target: &Target, name: &str) -> Option<School> {
    let kind = node.get("@type")?;
    let types = kind
        .as_array()
        .map_or_else(|| std::slice::from_ref(kind), Vec::as_slice);
    if !types.iter().filter_map(Value::as_str).any(school_type)
        || normalized(node.get("name")?.as_str()?) != normalized(name)
    {
        return None;
    }
    let address = node.get("address")?;
    let street = string(address, "streetAddress")?;
    let city = string(address, "addressLocality")?;
    let state = string(address, "addressRegion")?;
    let postal = string(address, "postalCode")?;
    if !state_matches(target, &state) || !valid_postal(&postal) {
        return None;
    }
    Some(School {
        id: target.context.school_id.clone()?,
        name: name.to_owned(),
        state: Some(state),
        city: Some(city),
        address: Some(street),
        postal_code: Some(postal),
        website: node
            .get("url")
            .and_then(Value::as_str)
            .and_then(|value| public_url(value).ok())
            .map(Into::into),
        public_email: None,
    })
}

fn school_type(kind: &str) -> bool {
    matches!(
        kind.rsplit('/').next(),
        Some(
            "School"
                | "HighSchool"
                | "MiddleSchool"
                | "ElementarySchool"
                | "EducationalOrganization"
        )
    )
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(str::to_owned)
}

fn valid_postal(value: &str) -> bool {
    matches!(value.len(), 5 | 10)
        && value.chars().enumerate().all(|(index, character)| {
            if index == 5 {
                character == '-'
            } else {
                character.is_ascii_digit()
            }
        })
}

fn state_matches(target: &Target, published: &str) -> bool {
    target
        .context
        .state
        .as_deref()
        .is_none_or(|state| state.eq_ignore_ascii_case(published))
}

fn html_schools(document: &Html, target: &Target, name: &str) -> Result<Vec<School>> {
    let addresses = selector("address, [itemprop='address']")?;
    let properties = [
        selector("[itemprop='streetAddress']")?,
        selector("[itemprop='addressLocality']")?,
        selector("[itemprop='addressRegion']")?,
        selector("[itemprop='postalCode']")?,
    ];
    let pattern = Regex::new(
        r"^(?P<street>\d[\w\s.#'/-]+?),\s+(?P<city>[A-Za-z][A-Za-z .'-]+),\s*(?P<state>[A-Z]{2})\s+(?P<zip>\d{5}(?:-\d{4})?)$",
    )?;
    Ok(document
        .select(&addresses)
        .filter_map(|block| {
            let content = text(block);
            let pieces = content.split_once(name)?;
            if !normalized(pieces.0).is_empty() {
                return None;
            }
            structured_address(block, target, name, &properties).or_else(|| {
                let remainder = pieces.1.trim_matches(|character: char| {
                    character.is_whitespace() || matches!(character, ':' | ',' | '-')
                });
                let found = pattern.captures(remainder)?;
                let state = found.name("state")?.as_str();
                let id = target.context.school_id.clone()?;
                state_matches(target, state).then(|| School {
                    id,
                    name: name.to_owned(),
                    state: Some(state.to_owned()),
                    city: found
                        .name("city")
                        .map(|value| value.as_str().trim().to_owned()),
                    address: found
                        .name("street")
                        .map(|value| value.as_str().trim().to_owned()),
                    postal_code: found.name("zip").map(|value| value.as_str().to_owned()),
                    website: None,
                    public_email: None,
                })
            })
        })
        .collect())
}

fn structured_address(
    block: ElementRef<'_>,
    target: &Target,
    name: &str,
    properties: &[Selector; 4],
) -> Option<School> {
    let [street, city, state, postal] = properties.each_ref().map(|property| {
        block
            .select(property)
            .next()
            .map(text)
            .filter(|value| !value.trim().is_empty())
    });
    let state = state?;
    let postal = postal?;
    if !state_matches(target, &state) || !valid_postal(&postal) {
        return None;
    }
    Some(School {
        id: target.context.school_id.clone()?,
        name: name.to_owned(),
        city: Some(city?),
        address: Some(street?),
        state: Some(state),
        postal_code: Some(postal),
        website: None,
        public_email: None,
    })
}
