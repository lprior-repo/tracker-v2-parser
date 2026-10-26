use std::sync::LazyLock;

use anyhow::{Result, ensure};
use regex::Regex;
use scraper::Html;
use serde::Deserialize;

use crate::model::{PageContext, PageKind, ParsedPage, Source, Target};

use super::{html, raw, targets};

static RESULT_FILES: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?s)meetResultFiles\s*=\s*(\[[^]]*\])"));

#[derive(Deserialize)]
struct ResultFile {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "isMeetPro")]
    is_meet_pro: u64,
    #[serde(default)]
    inline: bool,
}

pub(super) fn parse(target: &Target, body: &str, document: &Html) -> Result<ParsedPage> {
    let context = metadata(target, document)?;
    let inventory = RESULT_FILES
        .as_ref()
        .map_err(|error| anyhow::anyhow!("MileSplit inventory expression failed: {error}"))?;
    let mut page = if document
        .select(&html::selector("#meetResultsBody pre, pre")?)
        .next()
        .is_some()
    {
        raw::parse(
            &Target {
                context: context.clone(),
                ..target.clone()
            },
            document,
        )?
    } else {
        ParsedPage::default()
    };
    if let Some(literal) = inventory.captures(body).and_then(|capture| capture.get(1)) {
        let files: Vec<ResultFile> = serde_json::from_str(literal.as_str())?;
        if files.is_empty() {
            page.notes
                .push("Meet publishes an empty result-file inventory".to_owned());
        }
        return files.into_iter().try_fold(page, |page, file| {
            result_file(target, &context, &file, page)
        });
    }
    let root = document.root_element();
    let select = root.select(&html::selector("#ddResultsPage")?).next();
    if let Some(select) = select {
        let page =
            select
                .select(&html::selector("option[value]")?)
                .try_fold(page, |page, option| {
                    let href = option
                        .value()
                        .attr("value")
                        .ok_or_else(|| anyhow::anyhow!("result option missing URL"))?;
                    legacy_file(target, &context, href, page)
                })?;
        ensure!(
            !page.next.is_empty() || !page.notes.is_empty() || !page.performances.is_empty(),
            "MileSplit legacy result inventory has no readable files"
        );
        return Ok(page);
    }
    ensure!(
        !page.notes.is_empty() || !page.performances.is_empty(),
        "MileSplit meet has no recognized public result inventory or raw block"
    );
    Ok(page)
}

pub(super) fn metadata(target: &Target, document: &Html) -> Result<PageContext> {
    let meet_id = targets::path_id(&target.url, "meets")?;
    let Some(event) = html::schema(document, "SportsEvent")? else {
        return Ok(PageContext {
            subject_id: Some(meet_id),
            ..target.context.clone()
        });
    };
    if let Some(url) = html::field(&event, "url") {
        ensure!(
            targets::path_id(&url, "meets")? == meet_id,
            "MileSplit SportsEvent belongs to another meet"
        );
    }
    let sport = html::field(&event, "sport")
        .map(|sport| targets::sport(&sport))
        .transpose()?
        .or(target.context.sport);
    Ok(PageContext {
        subject_id: Some(meet_id),
        meet_name: html::field(&event, "name").or_else(|| target.context.meet_name.clone()),
        meet_date: html::field(&event, "startDate").or_else(|| target.context.meet_date.clone()),
        sport,
        ..target.context.clone()
    })
}

fn result_file(
    target: &Target,
    context: &PageContext,
    file: &ResultFile,
    mut page: ParsedPage,
) -> Result<ParsedPage> {
    if file.is_meet_pro != 0 {
        page.notes.push(format!(
            "Incomplete public access: subscription result file {} ({}) not fetched",
            file.id, file.name
        ));
        return Ok(page);
    }
    if file.inline {
        ensure!(
            !page.notes.is_empty() || !page.performances.is_empty(),
            "inline meet result has no readable raw block"
        );
        return Ok(page);
    }
    ensure!(file.id > 0, "invalid result-file ID");
    let mut url = targets::public_url(&target.url)?;
    let meet_id = context
        .subject_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("meet ID missing"))?;
    url.set_path(&format!("/meets/{meet_id}/results/{}/raw", file.id));
    url.set_query(None);
    page.next.push(Target::get(
        Source::MileSplit,
        PageKind::Results,
        url.to_string(),
        context.clone(),
    ));
    Ok(page)
}

fn legacy_file(
    target: &Target,
    context: &PageContext,
    href: &str,
    mut page: ParsedPage,
) -> Result<ParsedPage> {
    if href.is_empty() || href == "#" {
        return Ok(page);
    }
    let mut url = targets::public_url(&targets::linked(&target.url, href)?)?;
    let meet_id = targets::path_id(url.as_str(), "meets")?;
    ensure!(
        context.subject_id.as_deref() == Some(meet_id.as_str()),
        "result file belongs to another meet"
    );
    let result_id = match targets::path_id(url.as_str(), "results") {
        Ok(id) => id,
        Err(_) if url.path().ends_with("/results") => return Ok(page),
        Err(error) => return Err(error),
    };
    url.set_path(&format!("/meets/{meet_id}/results/{result_id}/raw"));
    url.set_query(None);
    page.next.push(Target::get(
        Source::MileSplit,
        PageKind::Results,
        url.to_string(),
        context.clone(),
    ));
    Ok(page)
}
