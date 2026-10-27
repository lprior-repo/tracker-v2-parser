use anyhow::{Result, ensure};
use scraper::ElementRef;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{details, html, targets};
use crate::model::{Athlete, ParsedPage, Performance, Target};

pub(super) fn parse(
    target: &Target,
    athlete: &Athlete,
    root: ElementRef<'_>,
    page: &mut ParsedPage,
) -> Result<()> {
    let rows = root
        .select(&html::selector("#stats .record[data-season][data-event]")?)
        .collect::<Vec<_>>();
    ensure!(rows.len() <= 10_000, "profile history exceeds 10000 rows");
    rows.into_iter().try_for_each(|row| {
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
        page.performances.push(Performance {
            athlete_id: athlete.id.clone(),
            is_relay: html::relay(&event),
            event, reported_status: crate::marks::failure_status(&mark).map(str::to_owned), mark,
            source_values: Some(json!({"kind": "career_result", "level": level, "meet_url": meet_url, "row": details::evidence(row)?})),
            sport, result_id: Some(format!("profile-history:{}:{:x}", athlete.id, Sha256::digest(row.html().as_bytes()))), source_row: None,
            school_id: athlete.school_id.clone(), meet_id,
            meet_name: html::value(row, ".location a[href*='/meets/']")?, date,
            wind: None, timing: None, course: None, is_personal_record: false,
            season: None, grade: None, gender: athlete.gender.clone(),
        });
        Ok(())
    })
}
