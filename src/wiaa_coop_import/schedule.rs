use anyhow::{Context, Result, ensure};
use scraper::{Html, Selector};
use url::Url;

use super::{
    evidence::{Association, Evidence},
    proof::{self, Program},
};

pub(super) struct Confirmation {
    pub sport: String,
    pub gender: String,
    pub coach: String,
    pub attribution: String,
}

impl Confirmation {
    pub fn matches(&self, program: &Program) -> bool {
        self.sport == program.sport && self.gender == program.gender
    }
}

pub(super) fn parse(
    association: &Association,
    evidence: &Evidence,
    body: &str,
) -> Result<Confirmation> {
    let url = Url::parse(&evidence.url)?;
    let query = url.query_pairs().collect::<Vec<_>>();
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("schools.wiaawi.org")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path() == "/Directory/Schedule/Index"
            && matches!(query.as_slice(), [(key,id)] if key == "TeamID" && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
            && evidence.method == "GET"
            && evidence.request_form.is_empty(),
        "current co-op schedule URL must contain one public TeamID"
    );
    let team_id = query
        .first()
        .map(|(_, id)| id.as_ref())
        .context("schedule TeamID absent")?;
    let document = Html::parse_document(body);
    let headings = select(".jumbotron h3")?;
    let names = document.select(&headings).map(text).collect::<Vec<_>>();
    ensure!(
        matches!(names.as_slice(), [name] if proof::normalized(name) == proof::normalized(&association.published_cooperative_name)),
        "current schedule cooperative name mismatch"
    );
    hidden(&document, "Options.SchoolID", &association.host_wiaa_org_id)?;
    hidden(&document, "Options.SchoolYear", "2026")?;
    hidden(&document, "Options.TeamID", team_id)?;
    hidden(&document, "TeamInfo.TeamID", team_id)?;
    let fields = select("div.row > div")?;
    let fields = document
        .select(&fields)
        .filter_map(|block| {
            let child = |name: &str| {
                block
                    .children()
                    .filter_map(scraper::ElementRef::wrap)
                    .find(|element| element.value().name() == name)
                    .map(text)
            };
            Some((child("label")?, child("h5")?))
        })
        .collect::<Vec<_>>();
    ensure!(
        field(&fields, "School Year")? == "2026-2027" && field(&fields, "Team Level")? == "Varsity",
        "current schedule year/level mismatch"
    );
    ensure!(
        proof::normalized(field(&fields, "IsCoop")?)
            == proof::normalized(&format!("Yes ({})", association.designated_contact_school)),
        "current schedule does not affirm the designated co-op host"
    );
    let sport = field(&fields, "Sport")?.to_owned();
    let gender = field(&fields, "Gender")?.to_owned();
    ensure!(
        matches!(sport.as_str(), "Cross Country" | "Track and Field")
            && matches!(gender.as_str(), "Boys" | "Girls"),
        "current schedule unsupported sport/gender"
    );
    let coach = field(&fields, "Head Coach")?.to_owned();
    ensure!(!coach.is_empty(), "current schedule head coach absent");
    Ok(Confirmation {
        sport,
        gender,
        coach,
        attribution: format!(
            "WIAA current 2026-2027 schedule affirms cooperative and designated host; {} SHA-256 {} captured {}",
            evidence.url, evidence.sha256, evidence.captured_utc
        ),
    })
}

fn hidden(document: &Html, name: &str, expected: &str) -> Result<()> {
    let selector = select(&format!("input[name='{name}'][value]"))?;
    let values = document.select(&selector).collect::<Vec<_>>();
    ensure!(
        !values.is_empty()
            && values
                .iter()
                .all(|value| value.value().attr("value") == Some(expected)),
        "current schedule hidden identity mismatch: {name}"
    );
    Ok(())
}

fn field<'a>(fields: &'a [(String, String)], label: &str) -> Result<&'a str> {
    let matches = fields
        .iter()
        .filter(|(key, _)| key == label)
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [value] => Ok(value),
        _ => anyhow::bail!("current schedule field missing or ambiguous: {label}"),
    }
}

fn text(element: scraper::ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn select(value: &str) -> Result<Selector> {
    Selector::parse(value).map_err(|error| anyhow::anyhow!("co-op schedule selector: {error}"))
}
