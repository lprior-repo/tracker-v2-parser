use anyhow::{Context, Result, ensure};
use scraper::{Html, Selector};

use super::evidence::{Association, Evidence};

pub(super) const REPORT_URL: &str = "https://schools.wiaawi.org/Reports/RunCoopListReport/";

pub(super) struct Program {
    pub cooperative: String,
    pub host: String,
    pub sport: String,
    pub gender: String,
    pub expiration: String,
    pub deadline: String,
    pub evidence: Evidence,
}

impl Program {
    pub fn role_suffix(&self) -> String {
        format!("{} {}", self.gender, self.sport)
    }

    pub fn confirmed(&self) -> bool {
        self.expiration.parse::<u32>().is_ok_and(|year| year > 2026)
    }

    pub fn attribution(&self) -> String {
        format!(
            "WIAA published 2026-2027 cooperative {}: {} {}; designated contact school {}; published Expire Yr {}; Deadline {}; report {} SHA-256 {} captured {}; POST {:?}",
            self.cooperative,
            self.gender,
            self.sport,
            self.host,
            self.expiration,
            self.deadline,
            self.evidence.url,
            self.evidence.sha256,
            self.evidence.captured_utc,
            self.evidence.request_form
        )
    }
}

pub(super) fn report(evidence: &Evidence, body: &str) -> Result<(String, Vec<Program>)> {
    ensure!(
        evidence.url == REPORT_URL && evidence.method == "POST",
        "association proof must be the official WIAA POST report"
    );
    let field = |key: &str| evidence.request_form.get(key).map(String::as_str);
    ensure!(
        field("YearSel") == Some("2026")
            && field("DivisionSel") == Some("-1")
            && field("Levels") == Some("1")
            && evidence.request_form.len() == 5,
        "association report scope must be 2026 and all high-school divisions"
    );
    let (sport, season) = match field("SportSel") {
        Some("XC") => ("Cross Country", "Fall"),
        Some("TR") => ("Track and Field", "Spring"),
        _ => anyhow::bail!("association report has unsupported sport"),
    };
    let gender = match field("GenderSel") {
        Some("M") => "Boys",
        Some("F") => "Girls",
        _ => anyhow::bail!("association report has unsupported gender"),
    };
    let document = Html::parse_document(body);
    let rows = selector("div.row.gridRowStyle")?;
    let labels = selector("label")?;
    let programs = document
        .select(&rows)
        .take(2001)
        .map(|row| {
            let cells = row
                .select(&labels)
                .map(|cell| normalized_text(&cell.text().collect::<Vec<_>>().join(" ")))
                .collect::<Vec<_>>();
            let [
                cooperative,
                row_sport,
                row_gender,
                row_season,
                _,
                host,
                expiration,
                deadline,
                _,
            ] = cells.as_slice()
            else {
                anyhow::bail!("association report row must contain nine published fields");
            };
            ensure!(
                row_sport == sport && row_gender == gender && row_season == season,
                "association report row conflicts with requested sport/gender/season"
            );
            ensure!(
                !cooperative.is_empty() && !host.is_empty(),
                "association report identity is empty"
            );
            Ok(Program {
                cooperative: cooperative.clone(),
                host: host.clone(),
                sport: sport.into(),
                gender: gender.into(),
                expiration: expiration.clone(),
                deadline: deadline.clone(),
                evidence: evidence.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !programs.is_empty() && programs.len() <= 2000,
        "association report has no recognized rows or exceeds capacity"
    );
    Ok((format!("{sport}:{gender}"), programs))
}

pub(super) fn mapping(association: &Association) -> Result<()> {
    ensure!(
        association.provider_school_state == "WI"
            && !association.provider_school_id.trim().is_empty(),
        "co-op requires an existing Wisconsin provider identity"
    );
    let provider = compact(&association.provider_school_name);
    let published = compact(&association.published_cooperative_name);
    let approved = matches!(
        (provider.as_str(), published.as_str()),
        (
            "bentonscalesmoundshullsburg",
            "bentonscalesmoundilshullsburg"
        ) | ("stambroseabundantlife", "saintambroseabundantlifechristian")
    );
    ensure!(
        provider == published || approved,
        "provider co-op label lacks an exact or explicitly reviewed published alias"
    );
    ensure!(
        association.host_capture.url
            == format!(
                "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID={}",
                association.host_wiaa_org_id
            )
            && association.host_capture.method == "GET"
            && association.host_capture.request_form.is_empty(),
        "host evidence must be the designated official WIAA school GET"
    );
    Ok(())
}

pub(super) fn normalized(value: &str) -> String {
    normalized_text(value).to_lowercase()
}

fn normalized_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn selector(value: &str) -> Result<Selector> {
    Selector::parse(value)
        .map_err(|error| anyhow::anyhow!("co-op selector {value}: {error}"))
        .with_context(|| format!("parsing association selector {value}"))
}
