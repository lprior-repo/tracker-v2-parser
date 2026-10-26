mod columns;
mod retained;

use anyhow::{Result, ensure};
use scraper::{ElementRef, Html};

use crate::model::{
    Athlete, PageContext, PageKind, ParsedPage, Performance, School, Source, Target,
};

use super::{html, meets, targets};

#[derive(Default)]
struct Reader {
    page: ParsedPage,
    event: Option<String>,
    round: Option<String>,
    columns: Option<columns::Columns>,
    qualified: bool,
    rows: usize,
    unidentified: usize,
}

#[derive(Clone, Copy)]
struct RowScope<'a> {
    target: &'a Target,
    context: &'a PageContext,
    ordinal: usize,
    event: &'a str,
    round: Option<&'a str>,
    raw: &'a str,
    line: &'a str,
}

pub(super) fn parse(target: &Target, document: &Html) -> Result<ParsedPage> {
    let context = meets::metadata(target, document)?;
    ensure!(
        context.sport.is_some(),
        "raw MileSplit results have no published or requested sport"
    );
    let pre = document
        .select(&html::selector("#meetResultsBody pre, pre")?)
        .next()
        .ok_or_else(|| anyhow::anyhow!("MileSplit result page missing public <pre> block"))?;
    let block = pre.inner_html();
    let mut reader = block
        .lines()
        .enumerate()
        .try_fold(Reader::default(), |reader, (line, raw)| {
            read_line(target, &context, line, raw, reader)
        })?;
    ensure!(
        reader.qualified && reader.rows > 0,
        "unqualified MileSplit raw result document or no readable result rows"
    );
    if reader.unidentified > 0 {
        reader.page.notes.push(format!("{} published result rows have no actual athlete profile ID; retained as lossless unassigned results, not name-matched or fabricated", reader.unidentified));
    }
    reader.page.notes.push(format!(
        "Read {} source-published fixed-width result rows",
        reader.rows
    ));
    Ok(reader.page)
}

fn read_line(
    target: &Target,
    context: &PageContext,
    ordinal: usize,
    raw: &str,
    mut reader: Reader,
) -> Result<Reader> {
    let fragment = Html::parse_fragment(raw);
    let line = fragment.root_element().text().collect::<String>();
    if line.trim().is_empty()
        || line.trim_start().starts_with("====")
        || line.trim_start().starts_with("----")
    {
        return Ok(reader);
    }
    if let Some(columns) = columns::header(&line) {
        reader.columns = Some(columns);
        reader.qualified = true;
        return Ok(reader);
    }
    if event_label(&line) {
        reader.event = Some(line.trim().to_owned());
        reader.columns = None;
        reader.round = None;
        return Ok(reader);
    }
    if round_label(&line) {
        reader.round = Some(line.trim().to_owned());
        return Ok(reader);
    }
    let Some(columns) = reader.columns else {
        return Ok(reader);
    };
    let Some(event) = reader.event.as_deref() else {
        return Ok(reader);
    };
    let scope = RowScope {
        target,
        context,
        ordinal,
        event,
        round: reader.round.as_deref(),
        raw,
        line: &line,
    };
    let school = observed_school(target, fragment.root_element(), &mut reader.page)?;
    let row = columns::row(&line, columns);
    reader.rows = reader.rows.saturating_add(1);
    let Some(row) = row else {
        reader
            .page
            .unassigned_results
            .push(retained::unassigned(&scope, None, school.as_deref())?);
        reader.page.notes.push(format!(
            "Incomplete raw column parsing at source line {}; original row retained losslessly",
            ordinal.saturating_add(1)
        ));
        reader.unidentified = reader.unidentified.saturating_add(1);
        return Ok(reader);
    };
    let identified = identified_row(
        &scope,
        &row,
        fragment.root_element(),
        school.as_deref(),
        &mut reader.page,
    )?;
    if !identified {
        reader.page.unassigned_results.push(retained::unassigned(
            &scope,
            Some(&row),
            school.as_deref(),
        )?);
        reader.unidentified = reader.unidentified.saturating_add(1);
    }
    Ok(reader)
}

fn observed_school(
    target: &Target,
    fragment: ElementRef<'_>,
    page: &mut ParsedPage,
) -> Result<Option<String>> {
    let Some(link) = fragment
        .select(&html::selector("a[href*='/teams/']")?)
        .next()
    else {
        return Ok(None);
    };
    let name = html::text(link);
    if name.is_empty() {
        return Ok(None);
    }
    let href = link
        .value()
        .attr("href")
        .ok_or_else(|| anyhow::anyhow!("raw school URL missing"))?;
    let url = targets::linked(&target.url, href)?;
    let id = targets::path_id(&url, "teams")?;
    if !page.schools.iter().any(|school| school.id == id) {
        page.schools.push(School {
            id: id.clone(),
            name,
            ..School::default()
        });
    }
    Ok(Some(id))
}

fn event_label(line: &str) -> bool {
    if line.starts_with(char::is_whitespace) {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    let gender = lower
        .split_whitespace()
        .any(|token| matches!(token, "boys" | "girls" | "men" | "women" | "mixed"));
    let event = [
        "meter",
        "metre",
        "mile",
        "3200m",
        "1600m",
        "800m",
        "100m",
        "200m",
        "400m",
        "5000m",
        "60m",
        "55m",
        "5k",
        "3k",
        "2k",
        "6k",
        "8k",
        "10k",
        "hurdle",
        "jump",
        "vault",
        "shot put",
        "discus",
        "javelin",
        "hammer",
        "weight throw",
    ]
    .iter()
    .any(|token| lower.contains(token));
    gender || event
}

fn identified_row(
    scope: &RowScope<'_>,
    row: &columns::Row<'_>,
    fragment: ElementRef<'_>,
    school: Option<&str>,
    page: &mut ParsedPage,
) -> Result<bool> {
    let RowScope {
        target,
        context,
        event,
        ..
    } = *scope;
    let Some(link) = fragment
        .select(&html::selector("a[href*='/athletes/']")?)
        .next()
    else {
        return Ok(false);
    };
    ensure!(
        html::text(link) == row.name.split_whitespace().collect::<Vec<_>>().join(" "),
        "raw athlete link is not owned by fixed-width name cell"
    );
    let href = link
        .value()
        .attr("href")
        .ok_or_else(|| anyhow::anyhow!("raw athlete URL missing"))?;
    let url = targets::linked(&target.url, href)?;
    let id = targets::path_id(&url, "athletes")?;
    let school = school.map(str::to_owned);
    let gender = event.split_whitespace().find_map(html::gender);
    let athlete = Athlete {
        id: id.clone(),
        name: row.name.to_owned(),
        school_id: school.clone(),
        gender: gender.clone(),
        grade: row.grade.map(str::to_owned),
        profile_url: Some(url.clone()),
        ..Athlete::default()
    };
    let next_context = PageContext {
        athlete_id: Some(id.clone()),
        state: None,
        school_id: school.clone(),
        school_name: None,
        ..context.clone()
    };
    page.next.push(Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        url,
        next_context,
    ));
    page.athletes.push(athlete);
    let sport = context
        .sport
        .ok_or_else(|| anyhow::anyhow!("raw result sport missing"))?;
    let result_id = Some(retained::result_id(scope)?);
    page.performances.push(Performance {
        athlete_id: id,
        event: event.to_owned(),
        mark: row.mark.to_owned(),
        sport,
        result_id,
        source_row: None,
        school_id: school,
        meet_id: context.subject_id.clone(),
        meet_name: context.meet_name.clone(),
        date: context.meet_date.clone(),
        wind: row.wind,
        timing: None,
        course: None,
        is_personal_record: false,
        is_relay: html::relay(event),
        season: context.season,
        grade: row.grade.map(str::to_owned),
        gender,
    });
    Ok(true)
}

fn round_label(line: &str) -> bool {
    matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "finals"
            | "final"
            | "prelims"
            | "preliminaries"
            | "semifinals"
            | "semi-finals"
            | "timed finals"
            | "qualifying"
    )
}
