use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use scraper::{ElementRef, Html};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::model::School;

use super::{MAX_PAGE_BYTES, ORIGIN, normalized, school_url, selector, text};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorySchool {
    pub organization_id: String,
    pub name: String,
    pub city: String,
    pub url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryMatch<'a> {
    Matched(&'a DirectorySchool),
    Missing,
    Uncorroborated,
    Ambiguous,
}

pub fn listing_requests(body: &str) -> Result<Vec<String>> {
    ensure!(body.len() <= MAX_PAGE_BYTES, "WIAA listing exceeds 8 MiB");
    ensure!(
        body.contains("/Directory/School/DirectoryLetter?LetterBtn="),
        "WIAA listing does not publish the expected directory action"
    );
    let document = Html::parse_document(body);
    let options = selector("select#Letter option[value]")?;
    let letters = document
        .select(&options)
        .filter_map(|option| option.value().attr("value"))
        .filter(|value| value.len() == 1 && value.bytes().all(|byte| byte.is_ascii_uppercase()))
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        letters.len() == 26,
        "WIAA listing must publish all 26 letter choices"
    );
    Ok(letters
        .into_iter()
        .map(|letter| format!("{ORIGIN}/Directory/School/DirectoryLetter?LetterBtn={letter}"))
        .collect())
}

pub fn parse_listing(body: &str) -> Result<Vec<DirectorySchool>> {
    ensure!(body.len() <= MAX_PAGE_BYTES, "WIAA listing exceeds 8 MiB");
    crate::sources::contacts::check_public_access(body, ORIGIN)?;
    let document = Html::parse_document(body);
    let tables = selector("table#tblSchools")?;
    let table_count = document.select(&tables).count();
    if published_empty(&document)? {
        ensure!(
            table_count == 0,
            "WIAA empty marker conflicts with a school table"
        );
        return Ok(Vec::new());
    }
    ensure!(
        table_count == 1,
        "WIAA school table is missing or ambiguous"
    );
    let rows = selector("#tblSchools tbody tr")?;
    let cells = selector("td")?;
    let links = selector("a.schoolLink[href]")?;
    let names = selector("h5")?;
    let entries = document
        .select(&rows)
        .take(1001)
        .map(|row| parse_row(row, &cells, &links, &names))
        .collect::<Result<Vec<_>>>()?;
    ensure!(entries.len() <= 1000, "WIAA letter table exceeds 1000 rows");
    entries
        .into_iter()
        .flatten()
        .try_fold(BTreeMap::new(), |mut entries, row| {
            if let Some(previous) = entries.insert(row.organization_id.clone(), row.clone()) {
                ensure!(
                    previous == row,
                    "WIAA listing contains conflicting organization identities"
                );
            }
            Ok(entries)
        })
        .map(|entries| entries.into_values().collect())
}

fn published_empty(document: &Html) -> Result<bool> {
    let messages = selector("#ajaxMsgText")?;
    let messages = document.select(&messages).map(text).collect::<Vec<_>>();
    if messages.is_empty() {
        return Ok(false);
    }
    ensure!(
        matches!(messages.as_slice(), [message] if message == "Nothing was found matching this search criteria"),
        "WIAA empty marker is invalid or ambiguous"
    );
    let letter = selector("input#SearchLetter[value]")?;
    let letters = document
        .select(&letter)
        .filter_map(|input| input.value().attr("value"))
        .collect::<Vec<_>>();
    ensure!(
        matches!(letters.as_slice(), [letter] if letter.len() == 1 && letter.bytes().all(|byte| byte.is_ascii_uppercase())),
        "WIAA empty letter is missing or ambiguous"
    );
    let kind = selector("input#SearchType[value]")?;
    let kinds = document
        .select(&kind)
        .filter_map(|input| input.value().attr("value"))
        .collect::<Vec<_>>();
    ensure!(
        matches!(kinds.as_slice(), ["1"]),
        "WIAA empty search type must be letter search"
    );
    Ok(true)
}

fn parse_row(
    row: ElementRef<'_>,
    cells: &scraper::Selector,
    links: &scraper::Selector,
    names: &scraper::Selector,
) -> Result<Option<DirectorySchool>> {
    let columns = row.select(cells).collect::<Vec<_>>();
    let [_, school, level, _, city] = columns.as_slice() else {
        anyhow::bail!("WIAA school row must have five published columns");
    };
    if normalized(&text(*level)) != "high school" {
        return Ok(None);
    }
    let links = school.select(links).collect::<Vec<_>>();
    let [link] = links.as_slice() else {
        anyhow::bail!("WIAA school row must have one published school link");
    };
    let name = link
        .select(names)
        .next()
        .map(text)
        .context("WIAA school name missing")?;
    let url = Url::parse(ORIGIN)?.join(link.value().attr("href").context("WIAA link missing")?)?;
    school_url(url.as_str())?;
    let organization_id = url
        .query_pairs()
        .next()
        .map(|(_, id)| id.into_owned())
        .context("WIAA orgID missing")?;
    let city = text(*city);
    ensure!(
        !name.is_empty() && !city.is_empty(),
        "WIAA school name and city are required"
    );
    Ok(Some(DirectorySchool {
        organization_id,
        name,
        city,
        url: url.into(),
    }))
}

#[must_use]
pub fn match_school<'a>(school: &School, entries: &'a [DirectorySchool]) -> DirectoryMatch<'a> {
    if school
        .state
        .as_deref()
        .is_none_or(|state| !state.eq_ignore_ascii_case("WI"))
    {
        return DirectoryMatch::Uncorroborated;
    }
    let Some(city) = school
        .city
        .as_deref()
        .filter(|city| !city.trim().is_empty())
    else {
        return DirectoryMatch::Uncorroborated;
    };
    let name = normalized(&school.name);
    let city = normalized(city);
    let matches = entries
        .iter()
        .filter(|entry| normalized(&entry.name) == name && normalized(&entry.city) == city)
        .take(2)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => DirectoryMatch::Missing,
        [entry] => DirectoryMatch::Matched(entry),
        _ => DirectoryMatch::Ambiguous,
    }
}
