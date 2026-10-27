use anyhow::{Result, ensure};
use scraper::ElementRef;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::model::{PageContext, ParsedPage, Target, UnassignedResult};

use super::html;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Title,
    Course,
    Date,
    Location,
    Event,
    Header,
    Results,
    ScoreHeader,
    Scores,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AthleticStage {
    Generator,
    Title,
    Updated,
    Event,
    Header,
    Results,
}

pub(super) fn is_athleticrunmeet(table: ElementRef<'_>) -> Result<bool> {
    let rows = html::selector("tr")?;
    let cells = html::selector("td, th")?;
    Ok(table
        .select(&rows)
        .next()
        .map(|row| row.select(&cells).map(html::text).collect::<Vec<_>>())
        .is_some_and(|values| matches!(values.as_slice(), [name] if name == "AthleticRUNMEET")))
}

pub(super) fn parse_athleticrunmeet(
    target: &Target,
    context: &PageContext,
    table: ElementRef<'_>,
    identity: &str,
) -> Result<ParsedPage> {
    let rows = html::selector("tr")?;
    let cells = html::selector("td, th")?;
    let mut stage = AthleticStage::Generator;
    let mut event = String::new();
    let mut section_rows = 0usize;
    let mut page = ParsedPage::default();
    for (ordinal, row) in table.select(&rows).enumerate() {
        let values: Vec<String> = row.select(&cells).map(html::text).collect();
        if values.iter().all(String::is_empty) {
            continue;
        }
        match stage {
            AthleticStage::Generator => {
                ensure!(
                    matches!(values.as_slice(), [name] if name == "AthleticRUNMEET"),
                    "unrecognized table generator"
                );
                stage = AthleticStage::Title;
            }
            AthleticStage::Title => {
                ensure!(
                    matches!(values.as_slice(), [title] if !title.is_empty()),
                    "unrecognized table title"
                );
                stage = AthleticStage::Updated;
            }
            AthleticStage::Updated => {
                ensure!(
                    matches!(values.as_slice(), [blank, updated] if blank.is_empty() && updated.starts_with("(updated ") && updated.ends_with(')')),
                    "unrecognized table update"
                );
                stage = AthleticStage::Event;
            }
            AthleticStage::Event => {
                event = athletic_event(&values)?;
                stage = AthleticStage::Header;
            }
            AthleticStage::Header => {
                ensure!(
                    matches!(values.as_slice(), [place, name, grade, team, mark] if place.eq_ignore_ascii_case("Place") && name.eq_ignore_ascii_case("Name") && grade.eq_ignore_ascii_case("Grade") && team.eq_ignore_ascii_case("Team") && mark.eq_ignore_ascii_case("Mark")),
                    "unrecognized result table header"
                );
                section_rows = 0;
                stage = AthleticStage::Results;
            }
            AthleticStage::Results => {
                if values.len() == 1 {
                    ensure!(section_rows > 0, "result table section has no rows");
                    event = athletic_event(&values)?;
                    stage = AthleticStage::Header;
                } else {
                    page.unassigned_results.push(athletic_row(
                        target, context, row, identity, &event, ordinal, &values,
                    )?);
                    section_rows = section_rows.saturating_add(1);
                }
            }
        }
    }
    ensure!(
        stage == AthleticStage::Results && section_rows > 0,
        "unrecognized or incomplete result table"
    );
    page.notes.push(format!("{} published table result rows have no actual athlete profile ID; retained as unassigned results", page.unassigned_results.len()));
    Ok(page)
}

fn athletic_event(values: &[String]) -> Result<String> {
    match values {
        [event] if !event.is_empty() => Ok(event.clone()),
        _ => anyhow::bail!("unrecognized result table section"),
    }
}

fn athletic_row(
    target: &Target,
    context: &PageContext,
    row: ElementRef<'_>,
    identity: &str,
    event: &str,
    ordinal: usize,
    values: &[String],
) -> Result<UnassignedResult> {
    let [place, name, grade, team, mark] = values else {
        anyhow::bail!("unrecognized result table row");
    };
    let sport = context
        .sport
        .ok_or_else(|| anyhow::anyhow!("table results have no sport"))?;
    ensure!(
        !name.is_empty() && !team.is_empty() && !mark.is_empty(),
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
    Ok(UnassignedResult {
        result_id: Some(format!("table:{id}")),
        source_row: None,
        event: event.to_owned(),
        mark: mark.clone(),
        sport,
        school_id: None,
        meet_id: context.subject_id.clone(),
        date: context.meet_date.clone(),
        is_relay: html::relay(event),
        members: Vec::new(),
        raw: json!({"source_url": target.url, "line_ordinal": ordinal.saturating_add(1), "published_html": published_html, "place": place, "name": name, "grade": grade, "team": team, "event": event, "meet_name": context.meet_name}),
    })
}

pub(super) fn is_spreadsheet(table: ElementRef<'_>) -> Result<bool> {
    let rows = html::selector("tr")?;
    let cells = html::selector("td, th")?;
    let Some(row) = table.select(&rows).next() else {
        return Ok(false);
    };
    let values: Vec<String> = row.select(&cells).map(html::text).collect();
    Ok(
        matches!(values.as_slice(), [title, blank_one, blank_two, blank_three]
        if !title.is_empty() && blank_one.is_empty() && blank_two.is_empty() && blank_three.is_empty()),
    )
}

pub(super) fn parse(
    target: &Target,
    context: &PageContext,
    table: ElementRef<'_>,
    identity: &str,
) -> Result<ParsedPage> {
    ensure!(context.sport.is_some(), "table results have no sport");
    let rows = html::selector("tr")?;
    let cells = html::selector("td, th")?;
    let mut stage = Stage::Title;
    let mut event = String::new();
    let mut page = ParsedPage::default();
    let mut scores = 0usize;
    for (ordinal, row) in table.select(&rows).enumerate() {
        let values: Vec<String> = row.select(&cells).map(html::text).collect();
        if values.iter().all(String::is_empty) {
            ensure!(
                matches!(stage, Stage::Event | Stage::Results | Stage::Scores),
                "unexpected empty table row"
            );
            continue;
        }
        match stage {
            Stage::Title => {
                ensure!(
                    matches!(values.as_slice(), [title, blank_one, blank_two, blank_three] if !title.is_empty() && blank_one.is_empty() && blank_two.is_empty() && blank_three.is_empty()),
                    "unrecognized table title"
                );
                stage = Stage::Course;
            }
            Stage::Course | Stage::Date | Stage::Location => {
                stage = metadata_stage(stage, &values)?;
            }
            Stage::Event => {
                let [
                    label,
                    blank_one,
                    blank_two,
                    blank_three,
                    blank_four,
                    blank_five,
                ] = values.as_slice()
                else {
                    anyhow::bail!("unrecognized table event");
                };
                ensure!(
                    !label.is_empty()
                        && blank_one.is_empty()
                        && blank_two.is_empty()
                        && blank_three.is_empty()
                        && blank_four.is_empty()
                        && blank_five.is_empty(),
                    "unrecognized table event"
                );
                event.clone_from(label);
                stage = Stage::Header;
            }
            Stage::Header => {
                ensure!(
                    matches!(values.as_slice(), [blank, place, name, team, time, points] if blank.is_empty() && place.eq_ignore_ascii_case("Place") && name.eq_ignore_ascii_case("Name") && team.eq_ignore_ascii_case("Team") && time.eq_ignore_ascii_case("Time") && points.eq_ignore_ascii_case("Points")),
                    "unrecognized result table header"
                );
                stage = Stage::Results;
            }
            Stage::Results => {
                if matches!(values.as_slice(), [blank_one, blank_two, label, blank_three, blank_four, blank_five] if blank_one.is_empty() && blank_two.is_empty() && label.ends_with(" Team Scores") && blank_three.is_empty() && blank_four.is_empty() && blank_five.is_empty())
                {
                    ensure!(
                        !page.unassigned_results.is_empty(),
                        "team scores without result rows"
                    );
                    stage = Stage::ScoreHeader;
                    continue;
                }
                page.unassigned_results.push(result_row(
                    target, context, row, identity, &event, ordinal, &values,
                )?);
            }
            Stage::ScoreHeader => {
                ensure!(
                    matches!(values.as_slice(), [blank, place, team, points, record, extra] if blank.is_empty() && place.eq_ignore_ascii_case("Place") && team.eq_ignore_ascii_case("Team") && points.eq_ignore_ascii_case("Points") && record.eq_ignore_ascii_case("(Win / Loss) Record") && extra.is_empty()),
                    "unrecognized team scores header"
                );
                stage = Stage::Scores;
            }
            Stage::Scores => {
                ensure!(valid_score(&values), "unrecognized team score row");
                scores = scores.saturating_add(1);
            }
        }
    }
    ensure!(
        !page.unassigned_results.is_empty()
            && (stage == Stage::Results || (stage == Stage::Scores && scores > 0)),
        "unrecognized or incomplete result table"
    );
    page.notes.push(format!("{} published table result rows have no actual athlete profile ID; retained as unassigned results", page.unassigned_results.len()));
    Ok(page)
}

fn metadata_stage(stage: Stage, values: &[String]) -> Result<Stage> {
    let [
        blank_one,
        blank_two,
        value,
        blank_three,
        blank_four,
        blank_five,
    ] = values
    else {
        anyhow::bail!("unrecognized table metadata row");
    };
    ensure!(
        blank_one.is_empty()
            && blank_two.is_empty()
            && blank_three.is_empty()
            && blank_four.is_empty()
            && blank_five.is_empty()
            && !value.is_empty(),
        "unrecognized table metadata row"
    );
    match stage {
        Stage::Course => {
            ensure!(
                value.eq_ignore_ascii_case("At this Course"),
                "unrecognized table course"
            );
            Ok(Stage::Date)
        }
        Stage::Date => Ok(Stage::Location),
        Stage::Location => Ok(Stage::Event),
        _ => anyhow::bail!("invalid metadata state"),
    }
}

fn result_row(
    target: &Target,
    context: &PageContext,
    row: ElementRef<'_>,
    identity: &str,
    event: &str,
    ordinal: usize,
    values: &[String],
) -> Result<UnassignedResult> {
    let [blank, place, name, team, mark, points] = values else {
        anyhow::bail!("unrecognized result table row");
    };
    ensure!(
        blank.is_empty()
            && !place.is_empty()
            && !name.is_empty()
            && !team.is_empty()
            && !mark.is_empty(),
        "incomplete result table row"
    );
    let sport = context
        .sport
        .ok_or_else(|| anyhow::anyhow!("table results have no sport"))?;
    let published_html = row.html();
    let digest = format!("{:x}", Sha256::digest(published_html.as_bytes()));
    let id = serde_json::to_string(&(
        context.subject_id.as_deref(),
        identity,
        event,
        ordinal.saturating_add(1),
        digest,
    ))?;
    Ok(UnassignedResult {
        result_id: Some(format!("table:{id}")),
        source_row: None,
        event: event.to_owned(),
        mark: mark.clone(),
        sport,
        school_id: None,
        meet_id: context.subject_id.clone(),
        date: context.meet_date.clone(),
        is_relay: html::relay(event),
        members: Vec::new(),
        raw: json!({"source_url": target.url, "line_ordinal": ordinal.saturating_add(1), "published_html": published_html, "place": place, "name": name, "team": team, "points": points, "event": event, "meet_name": context.meet_name}),
    })
}

fn valid_score(values: &[String]) -> bool {
    match values {
        [blank, place, team, points] => {
            blank.is_empty() && !place.is_empty() && !team.is_empty() && !points.is_empty()
        }
        [blank, place, team, points, record, extra] => {
            blank.is_empty()
                && !place.is_empty()
                && !team.is_empty()
                && !points.is_empty()
                && record.is_empty()
                && extra.is_empty()
        }
        _ => false,
    }
}
