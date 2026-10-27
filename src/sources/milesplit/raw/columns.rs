#[derive(Clone, Copy)]
pub(super) struct Columns {
    name: usize,
    grade: Option<usize>,
    team: usize,
    mark_start: usize,
    mark_end: usize,
    wind: Option<usize>,
}

pub(super) struct Row<'a> {
    pub name: &'a str,
    pub grade: Option<&'a str>,
    pub team: &'a str,
    pub mark: &'a str,
    pub wind: Option<f64>,
}

pub(super) fn header(line: &str) -> Option<Columns> {
    let first = line.split_whitespace().next()?;
    if !["Pl", "Place", "Name", "Athlete"]
        .iter()
        .any(|value| first.eq_ignore_ascii_case(value))
    {
        return None;
    }
    let name = label(line, &["Name", "Athlete"])?;
    let team = label(line, &["Team", "School"])?;
    let mark = label(line, &["Time", "Mark", "Finals"])?;
    let mark_width = line
        .chars()
        .skip(mark)
        .take_while(|ch| !ch.is_whitespace())
        .count();
    let mark_end = mark.checked_add(mark_width)?;
    let grade = label(line, &["Yr", "Year", "Grade"]);
    let mark_start = mark.saturating_sub(mark_width.saturating_add(1));
    (name < team && team < mark_start && grade.is_none_or(|grade| name < grade && grade < team))
        .then_some(Columns {
            name,
            grade,
            team,
            mark_start,
            mark_end,
            wind: label(line, &["Wind"]),
        })
}

fn label(line: &str, wanted: &[&str]) -> Option<usize> {
    line.char_indices()
        .enumerate()
        .find_map(|(column, (byte, ch))| {
            let is_start = byte == 0
                || line
                    .get(..byte)
                    .and_then(|head| head.chars().next_back())
                    .is_some_and(char::is_whitespace);
            if ch.is_whitespace() || !is_start {
                return None;
            }
            let token = line.get(byte..)?.split_whitespace().next()?;
            wanted
                .iter()
                .any(|wanted| token.eq_ignore_ascii_case(wanted))
                .then_some(column)
        })
}

fn byte(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(byte, _)| byte)
}

fn cell(line: &str, start: usize, end: usize) -> Option<&str> {
    line.get(byte(line, start)..byte(line, end)).map(str::trim)
}

pub(super) fn row(line: &str, columns: Columns) -> Option<Row<'_>> {
    let grade_start = columns.grade.map_or(columns.team, |grade| grade);
    let name = cell(line, columns.name, grade_start).filter(|name| !name.is_empty())?;
    let head = line.get(..byte(line, columns.mark_end))?;
    let mark = if head
        .chars()
        .next_back()
        .is_some_and(|ch| !ch.is_whitespace())
    {
        head.split_whitespace().next_back()?
    } else {
        ""
    };
    let token_start = head.chars().count().checked_sub(mark.chars().count())?;
    let previous = line.get(byte(line, columns.mark_end)..)?.chars().next();
    let placed = cell(line, 0, columns.name)?
        .split_whitespace()
        .next()
        .is_none_or(|place| place.parse::<u32>().is_ok() || place == "--");
    let numeric = mark.trim_start_matches(['J', 'j']);
    let status = [
        "DNF",
        "DNS",
        "DQ",
        "NH",
        "NM",
        "NT",
        "FOUL",
        "SCR",
        "NAN",
        "INF",
        "INFINITY",
        "-INF",
        "-INFINITY",
    ]
    .iter()
    .any(|status| mark.eq_ignore_ascii_case(status));
    let shaped = numeric.starts_with(|ch: char| ch.is_ascii_digit())
        && (token_start >= columns.mark_start || mark.contains([':', '.', '-', '\'']));
    if !placed
        || previous.is_some_and(|ch| !ch.is_whitespace())
        || !(shaped || status || mark.is_empty())
    {
        return None;
    }
    let grade = columns
        .grade
        .and_then(|start| cell(line, start, columns.team))
        .filter(|grade| !grade.is_empty() && *grade != "-");
    let team = cell(line, columns.team, token_start).map_or("", str::trim);
    let wind = columns
        .wind
        .and_then(|start| line.get(byte(line, start.saturating_sub(2))..))
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|wind| wind.parse::<f64>().ok())
        .filter(|wind| wind.is_finite());
    Some(Row {
        name,
        grade,
        team,
        mark,
        wind,
    })
}
