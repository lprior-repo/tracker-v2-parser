use anyhow::{Context, Result, ensure};
use scraper::Html;

use crate::model::{School, Target};

use super::{normalized, school_url, selector, text};

pub(super) fn parse(target: &Target, body: &str) -> Result<School> {
    let document = Html::parse_document(body);
    let titles = selector("#SchoolItem .JumboMain")?;
    let labels = selector("#schoolInfo .entryrowPadding")?;
    let label = selector("span")?;
    let value = selector("h5")?;
    let names = document.select(&titles).map(text).collect::<Vec<_>>();
    let [name] = names.as_slice() else {
        anyhow::bail!("WIAA school identity is missing or ambiguous");
    };
    let expected = target
        .context
        .school_name
        .as_deref()
        .context("school name required")?;
    ensure!(
        normalized(name) == normalized(expected),
        "WIAA detail school name does not match the source school"
    );
    let fields = document
        .select(&labels)
        .filter_map(|block| {
            let name = block.select(&label).next().map(text)?;
            let value = block.select(&value).next().map(text)?;
            (!value.is_empty()).then_some((name, value))
        })
        .collect::<Vec<_>>();
    let street = field(&fields, "Address")?;
    let city = field(&fields, "City")?;
    let state = field(&fields, "State")?;
    let postal = field(&fields, "Zip")?;
    ensure!(
        state == "WI" && target.context.state.as_deref() == Some("WI"),
        "WIAA detail requires corroborated Wisconsin state"
    );
    ensure!(valid_postal(postal), "WIAA postal code is invalid");
    ensure!(
        field(&fields, "Level")? == "High School",
        "WIAA detail is not a high school"
    );
    let url = school_url(&target.url)?;
    let expected_id = url
        .query_pairs()
        .next()
        .map(|(_, id)| id.into_owned())
        .context("WIAA orgID missing")?;
    let identities = selector("input[name='orgID'][value]")?;
    let identities = document.select(&identities).collect::<Vec<_>>();
    ensure!(
        !identities.is_empty()
            && identities
                .iter()
                .all(|input| input.value().attr("value") == Some(expected_id.as_str())),
        "WIAA detail organization ID conflicts with requested URL"
    );
    let second = fields
        .iter()
        .find(|(label, _)| label == "Address2")
        .map(|(_, value)| value);
    Ok(School {
        id: target
            .context
            .school_id
            .clone()
            .context("source school ID required")?,
        name: expected.to_owned(),
        state: Some(state.to_owned()),
        city: Some(city.to_owned()),
        address: Some(
            second.map_or_else(|| street.to_owned(), |second| format!("{street}, {second}")),
        ),
        postal_code: Some(postal.to_owned()),
        ..School::default()
    })
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Result<&'a str> {
    let values = fields
        .iter()
        .filter(|(label, _)| label == name)
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    match values.as_slice() {
        [value] => Ok(value),
        _ => anyhow::bail!("WIAA {name} field is missing or ambiguous"),
    }
}

fn valid_postal(value: &str) -> bool {
    matches!(value.len(), 5 | 10)
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 5 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}
