use std::sync::LazyLock;

use anyhow::{Result, ensure};
use regex::Regex;
use scraper::{ElementRef, Html};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::model::{PageContext, PageKind, ParsedPage, Source, Target, UnassignedResult};

use super::{html, manual, raw, targets};

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
    } else if let Some(table) = document
        .select(&html::selector("#meetResultsBody table")?)
        .next()
    {
        parse_table(target, &context, document, table)?
    } else {
        ParsedPage::default()
    };
    if let Some(literal) = inventory.captures(body).and_then(|capture| capture.get(1)) {
        let files: Vec<ResultFile> = serde_json::from_str(literal.as_str())?;
        if files.is_empty() {
            page.notes
                .push("Meet publishes an empty result-file inventory".to_owned());
        }
        let ids = files
            .iter()
            .filter(|file| file.is_meet_pro == 0 && file.id > 0)
            .map(|file| file.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let mut page = files.into_iter().try_fold(page, |page, file| {
            result_file(target, &context, &file, page)
        })?;
        if !ids.is_empty() {
            page.next.push(super::owned::target(
                target,
                &context,
                ids.into_iter().collect(),
            )?);
        }
        return Ok(page);
    }
    if let Some(select) = document
        .root_element()
        .select(&html::selector("#ddResultsPage")?)
        .next()
    {
        return legacy_inventory(target, &context, document, select, page);
    }
    ensure!(
        !page.notes.is_empty()
            || !page.performances.is_empty()
            || !page.unassigned_results.is_empty(),
        "MileSplit meet has no recognized public result inventory or raw block"
    );
    Ok(page)
}

fn legacy_inventory(
    target: &Target,
    context: &PageContext,
    document: &Html,
    select: ElementRef<'_>,
    page: ParsedPage,
) -> Result<ParsedPage> {
    let selected = selected_inline_result(target, document, &page)?;
    let selected_file = if let Some(href) = selected.as_deref() {
        let selected_url = targets::linked(&target.url, href)?;
        let file = targets::path_id(&selected_url, "results")?;
        Some((selected_url, file))
    } else {
        None
    };
    let mut page =
        select
            .select(&html::selector("option[value]")?)
            .try_fold(page, |page, option| {
                let href = option
                    .value()
                    .attr("value")
                    .ok_or_else(|| anyhow::anyhow!("result option missing URL"))?;
                if option.value().attr("selected").is_some() && selected.as_deref() == Some(href) {
                    return Ok(page);
                }
                legacy_file(target, context, href, page)
            })?;
    let mut ids = page
        .next
        .iter()
        .filter(|next| next.kind == PageKind::Results)
        .filter_map(|next| targets::path_id(&next.url, "results").ok())
        .collect::<std::collections::BTreeSet<_>>();
    if let Some((_, file)) = &selected_file {
        ids.insert(file.clone());
    }
    if !ids.is_empty() {
        let api_parent = page
            .next
            .iter()
            .find(|next| next.kind == PageKind::Results)
            .cloned()
            .or_else(|| {
                selected_file.as_ref().map(|(url, _)| Target {
                    url: url.clone(),
                    ..target.clone()
                })
            })
            .ok_or_else(|| anyhow::anyhow!("published result source missing"))?;
        page.next.push(super::owned::target(
            &api_parent,
            context,
            ids.into_iter().collect(),
        )?);
    }
    ensure!(
        !page.next.is_empty()
            || !page.notes.is_empty()
            || !page.performances.is_empty()
            || !page.unassigned_results.is_empty(),
        "MileSplit legacy result inventory has no readable files"
    );
    Ok(page)
}

pub(super) fn parse_result(target: &Target, document: &Html) -> Result<ParsedPage> {
    if document
        .select(&html::selector("#meetResultsBody pre, pre")?)
        .next()
        .is_some()
    {
        return raw::parse(target, document);
    }
    let table = document
        .select(&html::selector("#meetResultsBody table")?)
        .next()
        .ok_or_else(|| anyhow::anyhow!("MileSplit result page missing public result block"))?;
    let context = metadata(target, document)?;
    parse_table(target, &context, document, table)
}

fn parse_table(
    target: &Target,
    context: &PageContext,
    document: &Html,
    table: ElementRef<'_>,
) -> Result<ParsedPage> {
    let identity = table_identity(target, document)?;
    if manual::is_athleticrunmeet(table)? {
        manual::parse_athleticrunmeet(target, context, table, &identity)
    } else if manual::is_spreadsheet(table)? {
        manual::parse(target, context, table, &identity)
    } else {
        table_results(target, context, table, &identity)
    }
}

fn table_identity(target: &Target, document: &Html) -> Result<String> {
    let Some(canonical) = html::attribute(document.root_element(), "link[rel=canonical]", "href")?
    else {
        return Ok(target.url.clone());
    };
    let canonical = targets::public_url(&targets::linked(&target.url, &canonical)?)?;
    let meet = targets::path_id(&target.url, "meets")?;
    let file = targets::path_id(canonical.as_str(), "results");
    if !canonical.path().ends_with("/raw")
        || targets::path_id(canonical.as_str(), "meets")
            .ok()
            .as_deref()
            != Some(meet.as_str())
        || file.is_err()
    {
        return Ok(target.url.clone());
    }
    let file = file?;
    let requested = targets::path_id(&target.url, "results");
    let matches_request = matches!(requested.as_deref(), Ok(value) if value == file);
    let selected = html::attribute(
        document.root_element(),
        "#ddResultsPage option[selected][value]",
        "value",
    )?;
    let matches_selected = if requested.is_err() {
        if let Some(selected) = selected {
            let selected = targets::public_url(&targets::linked(&target.url, &selected)?)?;
            selected.host_str() == canonical.host_str()
                && targets::path_id(selected.as_str(), "meets").ok().as_deref()
                    == Some(meet.as_str())
                && targets::path_id(selected.as_str(), "results")
                    .ok()
                    .as_deref()
                    == Some(file.as_str())
        } else {
            false
        }
    } else {
        false
    };
    if matches_request || matches_selected {
        Ok(format!("milesplit-file:{meet}:{file}"))
    } else {
        Ok(target.url.clone())
    }
}

fn selected_inline_result(
    target: &Target,
    document: &Html,
    page: &ParsedPage,
) -> Result<Option<String>> {
    if page.unassigned_results.is_empty() && page.performances.is_empty() {
        return Ok(None);
    }
    let Some(canonical) = html::attribute(document.root_element(), "link[rel=canonical]", "href")?
    else {
        return Ok(None);
    };
    let Some(option) = document
        .select(&html::selector("#ddResultsPage option[selected][value]")?)
        .next()
    else {
        return Ok(None);
    };
    let Some(href) = option.value().attr("value") else {
        return Ok(None);
    };
    let canonical = targets::public_url(&targets::linked(&target.url, &canonical)?)?;
    let selected = targets::public_url(&targets::linked(&target.url, href)?)?;
    let canonical_file = targets::path_id(canonical.as_str(), "results");
    let selected_file = targets::path_id(selected.as_str(), "results");
    let canonical_meet = targets::path_id(canonical.as_str(), "meets")?;
    let target_meet = targets::path_id(&target.url, "meets")?;
    let selected_meet = targets::path_id(selected.as_str(), "meets")?;
    if !canonical.path().ends_with("/raw")
        || canonical.host_str() != selected.host_str()
        || canonical_meet != target_meet
        || canonical_meet != selected_meet
        || canonical_file.is_err()
        || canonical_file.ok() != selected_file.ok()
    {
        return Ok(None);
    }
    Ok(Some(href.to_owned()))
}

const fn table_header(place: &str, grade: &str, team: &str, mark: &str) -> bool {
    place.eq_ignore_ascii_case("place name")
        && grade.eq_ignore_ascii_case("grade")
        && team.eq_ignore_ascii_case("team")
        && mark.eq_ignore_ascii_case("time")
}

fn table_results(
    target: &Target,
    context: &PageContext,
    table: ElementRef<'_>,
    identity: &str,
) -> Result<ParsedPage> {
    let sport = context
        .sport
        .ok_or_else(|| anyhow::anyhow!("table results have no sport"))?;
    let rows = html::selector("tr")?;
    let cells = html::selector("td, th")?;
    let mut page = ParsedPage::default();
    let mut event = None::<String>;
    let mut header = false;
    let mut scoring = false;
    for (ordinal, row) in table.select(&rows).enumerate() {
        let values: Vec<String> = row.select(&cells).map(html::text).collect();
        if values.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        if values.len() == 1 {
            let value = values
                .first()
                .ok_or_else(|| anyhow::anyhow!("missing table cell"))?
                .trim();
            if scoring {
                continue;
            }
            ensure!(
                event.is_none() || header,
                "unrecognized result table section"
            );
            if value.eq_ignore_ascii_case("Team Scoring:") {
                scoring = true;
            } else {
                event = Some(value.to_owned());
                header = false;
            }
            continue;
        }
        ensure!(
            !scoring && values.len() == 4,
            "unrecognized result table row"
        );
        let [place_name, grade, team, mark] = values.as_slice() else {
            anyhow::bail!("unrecognized result table columns");
        };
        if table_header(place_name, grade, team, mark) {
            ensure!(
                event.is_some() && !header,
                "result table header without event"
            );
            header = true;
            continue;
        }
        ensure!(header, "unrecognized result table header");
        let event = event
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("result row missing event"))?;
        let (place, name) = place_name
            .split_once(char::is_whitespace)
            .ok_or_else(|| anyhow::anyhow!("result row missing place or name"))?;
        ensure!(
            !place.is_empty() && !name.trim().is_empty() && !mark.trim().is_empty(),
            "incomplete result table row"
        );
        let published_html = row.html();
        let digest = format!("{:x}", Sha256::digest(published_html.as_bytes()));
        let id = serde_json::to_string(&(
            context.subject_id.as_deref(),
            identity,
            event,
            ordinal.saturating_add(1),
            digest,
        ))?;
        page.unassigned_results.push(UnassignedResult {
            result_id: Some(format!("table:{id}")),
            source_row: None,
            event: event.to_owned(),
            mark: mark.trim().to_owned(),
            sport,
            school_id: None,
            meet_id: context.subject_id.clone(),
            date: context.meet_date.clone(),
            is_relay: html::relay(event),
            members: Vec::new(),
            raw: json!({
                "source_url": target.url,
                "line_ordinal": ordinal.saturating_add(1),
                "published_html": published_html,
                "place": place,
                "name": name.trim(),
                "grade": grade,
                "team": team,
                "event": event,
                "meet_name": context.meet_name,
            }),
        });
    }
    ensure!(
        header && !page.unassigned_results.is_empty(),
        "unrecognized or empty result table"
    );
    page.notes.push(format!("{} published table result rows have no actual athlete profile ID; retained as unassigned results", page.unassigned_results.len()));
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
