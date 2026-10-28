use anyhow::{Context, Result, ensure};
use scraper::{ElementRef, Html};
use serde_json::{Value, json};

use super::super::{html, targets};
use crate::model::{PageKind, ParsedPage, Target};

pub(super) fn values(
    target: &Target,
    document: &Html,
    schema_gender: Option<&str>,
    progression_gender: Option<&str>,
) -> Result<Value> {
    let root = document.root_element();
    let page = json!({
        "sections": elements(root, ".hero, .athlete-info, #bio, #biography, .athlete-bio, .biography, #progression, .record-box, .profile-navigation, section, article, main, #videos, #photos, #articles, .related")?,
        "pr_rows": elements(root, "#prTable tr[data-pr-mode]")?,
        "result_rows": elements(root, "#stats .record, #progression tr")?,
        "seasons": elements(root, "#stats .season > h4")?,
        "json_ld": json_ld(root)?,
        "gender_claims": {"schema_person": schema_gender, "progression": progression_gender},
        "progression": progression(root)?,
        "links": elements(root, "a[href]")?,
        "media": elements(root, "img, video, audio, source, iframe, .video, .photo")?,
    });
    Ok(json!({"pages": {target.url.clone(): page}}))
}

pub(super) fn evidence(element: ElementRef<'_>) -> Result<Value> {
    Ok(json!({
        "masked": html::masked(element)?,
        "text": html::text(element),
        "html": element.html(),
        "attributes": element.value().attrs().map(|(key, value)| (key.to_owned(), Value::String(value.to_owned()))).collect::<serde_json::Map<_, _>>(),
    }))
}

fn elements(root: ElementRef<'_>, selector: &str) -> Result<Vec<Value>> {
    let selected = root.select(&html::selector(selector)?).collect::<Vec<_>>();
    ensure!(
        selected.len() <= 10_000,
        "profile section exceeds 10000 elements"
    );
    selected.into_iter().map(evidence).collect()
}

fn json_ld(root: ElementRef<'_>) -> Result<Vec<Value>> {
    root.select(&html::selector("script[type='application/ld+json']")?)
        .map(|script| serde_json::from_str(&script.inner_html()).context("invalid profile JSON-LD"))
        .collect()
}

fn progression(root: ElementRef<'_>) -> Result<Vec<Value>> {
    root.select(&html::selector("script")?)
        .flat_map(|script| {
            script
                .inner_html()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter_map(|line| {
            line.trim()
                .strip_prefix("var progressionTrackerData")
                .map(str::to_owned)
        })
        .map(|assignment| {
            let value = assignment
                .trim()
                .strip_prefix('=')
                .context("invalid progression assignment")?
                .trim()
                .trim_end_matches(';');
            serde_json::from_str(value).context("invalid public progression JSON")
        })
        .collect()
}

pub(super) fn enqueue(target: &Target, root: ElementRef<'_>, page: &mut ParsedPage) -> Result<()> {
    let athlete_id = targets::path_id(&target.url, "athletes")?;
    let selector =
        html::selector(".profile-navigation a[href], .pagination a[href], a[rel='next']")?;
    let links = root
        .select(&selector)
        .filter_map(|link| link.value().attr("href"));
    let next = links
        .map(|href| targets::linked(&target.url, href))
        .collect::<Result<Vec<_>>>()?;
    let next = next
        .into_iter()
        .filter(|url| {
            targets::path_id(url, "athletes").is_ok_and(|id| id == athlete_id) && url != &target.url
        })
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        next.len() <= 32,
        "profile published frontier exceeds 32 links"
    );
    page.next.extend(next.into_iter().map(|url| {
        Target::get(
            target.source,
            PageKind::Athlete,
            url,
            target.context.clone(),
        )
    }));
    Ok(())
}

pub(super) fn progression_gender(root: ElementRef<'_>) -> Result<Option<String>> {
    Ok(root.select(&html::selector("script")?).find_map(|script| {
        let body = script.inner_html();
        if !body.contains("progressionTrackerData") {
            return None;
        }
        body.lines().find_map(|line| {
            let assignment = line.trim().strip_prefix("var gender")?;
            let value = assignment.trim_start().strip_prefix('=')?.trim();
            let value = value.trim_end_matches(';').trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .or_else(|| {
                    value
                        .strip_prefix('\'')
                        .and_then(|value| value.strip_suffix('\''))
                })?;
            html::gender(value)
        })
    }))
}
