use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    AthleticNet,
    MileSplit,
}

impl Source {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AthleticNet => "athleticnet",
            Self::MileSplit => "milesplit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sport {
    Outdoor,
    Indoor,
    CrossCountry,
}

impl Sport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outdoor => "outdoor",
            Self::Indoor => "indoor",
            Self::CrossCountry => "xc",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gender {
    Boys,
    Girls,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoachRole {
    HeadCoach,
    AssistantCoach,
    AthleticDirector,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    Directory,
    Team,
    Roster,
    Athlete,
    Meet,
    Results,
    Contacts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageContext {
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_state: Option<String>,
    pub school_id: Option<String>,
    pub school_name: Option<String>,
    pub athlete_id: Option<String>,
    pub sport: Option<Sport>,
    pub season: Option<i32>,
    pub subject_id: Option<String>,
    pub meet_name: Option<String>,
    pub meet_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result_set_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitionEvidence {
    pub source: Source,
    pub meet_id: String,
    pub sport: Sport,
    pub meet: serde_json::Value,
    pub divisions: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub source: Source,
    pub kind: PageKind,
    pub url: String,
    #[serde(default)]
    pub context: PageContext,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub browser_origin: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub competition_evidence: Option<CompetitionEvidence>,
}

impl Target {
    #[must_use]
    pub const fn get(source: Source, kind: PageKind, url: String, context: PageContext) -> Self {
        Self {
            source,
            kind,
            url,
            context,
            method: None,
            body: None,
            browser_origin: None,
            token_url: None,
            competition_evidence: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct School {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub name: String,
    pub state: Option<String>,
    pub city: Option<String>,
    pub address: Option<String>,
    pub postal_code: Option<String>,
    pub website: Option<String>,
    pub public_email: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Athlete {
    pub id: String,
    pub name: String,
    pub school_id: Option<String>,
    pub state: Option<String>,
    pub gender: Option<String>,
    pub grade: Option<String>,
    pub graduation_year: Option<i32>,
    pub profile_url: Option<String>,
    #[serde(default)]
    pub same_as: Vec<String>,
    #[serde(default)]
    pub needs_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_values: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Performance {
    pub athlete_id: String,
    pub event: String,
    pub mark: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_values: Option<serde_json::Value>,
    pub sport: Sport,
    pub result_id: Option<String>,
    #[serde(default)]
    pub source_row: Option<String>,
    pub school_id: Option<String>,
    pub meet_id: Option<String>,
    pub meet_name: Option<String>,
    pub date: Option<String>,
    pub wind: Option<f64>,
    pub timing: Option<String>,
    pub course: Option<String>,
    pub is_personal_record: bool,
    pub is_relay: bool,
    pub season: Option<i32>,
    pub grade: Option<String>,
    pub gender: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coach {
    pub school_id: String,
    pub name: Option<String>,
    pub role: String,
    pub email: Option<String>,
    pub sport: Option<Sport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnassignedResult {
    pub result_id: Option<String>,
    #[serde(default)]
    pub source_row: Option<String>,
    pub event: String,
    pub mark: String,
    pub sport: Sport,
    pub school_id: Option<String>,
    pub meet_id: Option<String>,
    pub date: Option<String>,
    pub is_relay: bool,
    pub members: Vec<String>,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedPage {
    pub schools: Vec<School>,
    pub athletes: Vec<Athlete>,
    pub performances: Vec<Performance>,
    #[serde(default)]
    pub unassigned_results: Vec<UnassignedResult>,
    pub coaches: Vec<Coach>,
    pub next: Vec<Target>,
    pub notes: Vec<String>,
}

#[derive(Debug)]
pub struct Capture {
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub schools: i64,
    pub athletes: i64,
    pub performances: i64,
    pub coaches: i64,
    pub jobs: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencedPerformance {
    pub source: Source,
    pub url: String,
    pub performance: Performance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AthleteProfileDetails {
    pub source: Source,
    pub source_id: String,
    pub url: Option<String>,
    pub source_values: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AthleteRecord {
    pub id: i64,
    pub name: String,
    pub gender: Option<String>,
    pub grade: Option<String>,
    pub graduation_year: Option<i32>,
    pub needs_review: bool,
    pub schools: Vec<School>,
    pub coaches: Vec<Coach>,
    pub profiles: Vec<String>,
    pub performances: Vec<EvidencedPerformance>,
    #[serde(default)]
    pub profile_details: Vec<AthleteProfileDetails>,
}
