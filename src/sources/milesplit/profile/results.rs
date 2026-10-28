use anyhow::{Result, ensure};
use scraper::ElementRef;
use serde_json::json;

use super::{details, history, html, targets};
use crate::model::{Athlete, ParsedPage, Performance, School, Target};

pub(super) fn parse(
    target: &Target,
    athlete: &mut Athlete,
    root: ElementRef<'_>,
    page: &mut ParsedPage,
) -> Result<()> {
    let rows = root
        .select(&html::selector("#stats .record[data-season][data-event]")?)
        .collect::<Vec<_>>();
    ensure!(rows.len() <= 10_000, "profile history exceeds 10000 rows");
    let identities = history::identities(target, &athlete.id, &rows)?;
    rows.into_iter().zip(identities).try_for_each(|(row, identity)| {
        let mode = row.value().attr("data-season").ok_or_else(|| anyhow::anyhow!("result season missing"))?;
        let Ok(sport) = targets::sport(mode) else {
            page.notes.push(format!("Published profile history mode {mode:?} retained as source evidence without guessing sport"));
            return Ok(());
        };
        let Some(mark_cell) = row.select(&html::selector(".seed")?).next() else { return Ok(()); };
        let mark = html::text(mark_cell);
        if html::masked(mark_cell)? || mark.is_empty() {
            return Ok(());
        }
        let event = row.value().attr("data-event").ok_or_else(|| anyhow::anyhow!("result event missing"))?.to_owned();
        let meet_href = html::attribute(row, ".location a[href*='/meets/']", "href")?;
        let meet_url = meet_href.as_deref().map(|href| targets::linked(&target.url, href)).transpose()?;
        let meet_id = meet_url.as_deref().map(|url| targets::path_id(url, "meets")).transpose()?;
        let date = html::value(row, ".date")?.filter(|date| !date.is_empty()).map(|date| html::date(&date));
        let level = row.value().attr("data-level");
        let (school_id, school_evidence) = represented_school(target, row, page)?;
        if identity.basis == "ambiguous" {
            athlete.needs_review = true;
            page.notes.push(format!("Ambiguous published career result identity for athlete {}; retained separately without inferring that rows describe the same result", athlete.id));
        }
        page.performances.push(Performance {
            athlete_id: athlete.id.clone(),
            is_relay: html::relay(&event),
            event, reported_status: crate::marks::failure_status(&mark).map(str::to_owned), mark,
            source_values: Some(json!({"kind": "career_result", "level": level, "meet_url": meet_url, "identity_basis": identity.basis, "school_attribution": if school_id.is_some() { "row_link" } else { "not_published" }, "school_evidence": school_evidence, "row": details::evidence(row)?})),
            sport, result_id: Some(identity.id), source_row: None,
            school_id, meet_id,
            meet_name: html::value(row, ".location a[href*='/meets/']")?, date,
            wind: None, timing: None, course: None, is_personal_record: false,
            season: None, grade: None, gender: athlete.gender.clone(),
        });
        Ok(())
    })
}

pub(super) fn represented_school(
    target: &Target,
    row: ElementRef<'_>,
    page: &mut ParsedPage,
) -> Result<(Option<String>, Option<serde_json::Value>)> {
    let Some(link) = row.select(&html::selector("a[href*='/teams/']")?).next() else {
        return Ok((None, None));
    };
    let href = link
        .value()
        .attr("href")
        .ok_or_else(|| anyhow::anyhow!("represented school link missing"))?;
    let url = targets::linked(&target.url, href)?;
    let id = targets::path_id(&url, "teams")?;
    let mut evidence = details::evidence(link)?;
    let fields = evidence
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("represented school evidence is not an object"))?;
    fields.insert("school_id".to_owned(), json!(id));
    if !page.schools.iter().any(|school| school.id == id) {
        page.schools.push(School {
            id: id.clone(),
            name: html::text(link),
            ..School::default()
        });
    }
    Ok((Some(id), Some(evidence)))
}
