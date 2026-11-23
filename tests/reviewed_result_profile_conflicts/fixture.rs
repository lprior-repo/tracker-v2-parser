use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{self, Artifact, OperationSpec, Workspace},
    model::{
        Athlete, Capture, PageContext, PageKind, ParsedPage, Performance, School, Source, Sport,
        Target, UnassignedResult,
    },
};

use crate::support::Scenario;

pub const PROFILE_ID: &str = "14474869";
pub const PROFILE_NAME: &str = "Casey Runner";
pub const ALIAS: &str = "Casey Runners";
const ALIAS_ID: &str = "14474870";
const SCHOOL: &str = "Fall Creek";
const SCHOOL_ID: &str = "fall-creek-wi";
const FILE_IDENTITY: &str = "milesplit-file:790295:1336298";
const RESULT_EVENT: &str = "Girls 5000 Meter";
const RESULT_URL: &str =
    "https://wi.milesplit.com/meets/790295-fall-creek-invitational/results/1336298/raw";
const MEET_ID: &str = "790295";
const MEET_DATE: &str = "2026-10-24";
pub const PRIOR_OWNED_ID: &str = "prior-owned-race";

fn profile_url(source_id: &str) -> String {
    format!("https://wi.milesplit.com/athletes/{source_id}-casey-runner")
}

async fn persist(database: &Database, target: &Target, page: &ParsedPage) -> Result<()> {
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".to_owned(),
        body: serde_json::to_vec(page)?,
    };
    database.persist(None, target, &capture, page).await?;
    Ok(())
}

pub async fn observe_profile(
    database: &Database,
    source_id: &str,
    name: &str,
    gender: &str,
) -> Result<()> {
    let url = profile_url(source_id);
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        url.clone(),
        PageContext::default(),
    );
    let page = ParsedPage {
        athletes: vec![Athlete {
            id: source_id.to_owned(),
            name: name.to_owned(),
            profile_url: Some(url),
            gender: Some(gender.to_owned()),
            grade: Some("12".to_owned()),
            graduation_year: Some(2027),
            state: Some("WI".to_owned()),
            school_id: Some(SCHOOL_ID.to_owned()),
            ..Athlete::default()
        }],
        schools: vec![School {
            id: SCHOOL_ID.to_owned(),
            name: SCHOOL.to_owned(),
            state: Some("WI".to_owned()),
            ..School::default()
        }],
        ..ParsedPage::default()
    };
    persist(database, &target, &page).await
}

pub async fn approve_alias(database: &Database, test: &Scenario) -> Result<()> {
    observe_profile(database, ALIAS_ID, ALIAS, "F").await?;
    let profiles: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source_id,athlete_id FROM athlete_sources WHERE source='milesplit' ORDER BY source_id",
    )
    .fetch_all(database.pool())
    .await?;
    let [(first, canonical), (other, separate)] = profiles.as_slice() else {
        anyhow::bail!("expected the original and alternate profiles")
    };
    assert_ne!(canonical, separate);
    let bytes = serde_json::to_vec(&json!({
        "schema_revision":1,"canonical_athlete_id":canonical,"preferred_name":PROFILE_NAME,
        "accepted_names":[PROFILE_NAME,ALIAS],"reviewed_by":"user","reason":"Verified fixture aliases",
        "profiles":[{"source":"milesplit","source_id":first,"expected_athlete_id":canonical},
                    {"source":"milesplit","source_id":other,"expected_athlete_id":separate}]
    }))?;
    let root = test.directory().join("approved-identity");
    std::fs::create_dir_all(&root)?;
    let artifact = stage(&root, "approved-identity.json", &bytes)?;
    let spec = OperationSpec::ReviewedIdentity { manifest: artifact };
    let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
    let workspace = Workspace::open(&root, "approved-identity", &spec)?;
    let report = operations::execute_import(&workspace, &spec, &test.url, &blocking).await?;
    assert_eq!(report["canonical_athlete_id"], *canonical);
    assert_eq!(report["profile_count"], 2);
    Ok(())
}

pub struct ResultFixture {
    pub result_id: String,
    pub mark: &'static str,
    pub ordinal: i64,
    pub published: String,
}

impl ResultFixture {
    pub fn new(ordinal: i64, mark: &'static str) -> Self {
        let published = format!("{ordinal:4} {PROFILE_NAME:25} SR {SCHOOL:40} {mark}");
        let digest = format!("{:x}", Sha256::digest(published.as_bytes()));
        let result_id = format!(
            "raw:{}",
            json!([MEET_ID, FILE_IDENTITY, RESULT_EVENT, null, ordinal, digest])
        );
        Self {
            result_id,
            mark,
            ordinal,
            published,
        }
    }

    pub async fn persist(&self, database: &Database) -> Result<()> {
        self.persist_capture(database, None, false).await
    }

    pub async fn persist_capture(
        &self,
        database: &Database,
        extra: Option<&Self>,
        include_owned: bool,
    ) -> Result<()> {
        let target = Target::get(
            Source::MileSplit,
            PageKind::Results,
            RESULT_URL.to_owned(),
            PageContext::default(),
        );
        let page = ParsedPage {
            unassigned_results: std::iter::once(self).chain(extra).map(Self::row).collect(),
            performances: include_owned.then(prior_owned).into_iter().collect(),
            ..ParsedPage::default()
        };
        persist(database, &target, &page).await
    }

    fn row(&self) -> UnassignedResult {
        UnassignedResult {
            result_id: Some(self.result_id.clone()),
            source_row: None,
            event: RESULT_EVENT.to_owned(),
            mark: self.mark.to_owned(),
            sport: Sport::CrossCountry,
            school_id: None,
            meet_id: Some(MEET_ID.to_owned()),
            date: Some(MEET_DATE.to_owned()),
            is_relay: false,
            members: Vec::new(),
            raw: json!({"kind":"raw_result","name":PROFILE_NAME,"team":SCHOOL,"grade":"SR",
                "published_file_identity":FILE_IDENTITY,"line_ordinal":self.ordinal,
                "published_html":self.published,"meet_name":"Fall Creek Invitational"}),
        }
    }

    pub async fn import(&self, test: &Scenario, operation_id: &str) -> Result<Value> {
        let root = test.directory().join("profile-conflict-artifacts");
        std::fs::create_dir_all(&root)?;
        let document = self.manifest(&root, test).await?;
        let artifact = stage(&root, "result-link.json", &serde_json::to_vec(&document)?)?;
        let spec = OperationSpec::ReviewedResultLinks { manifest: artifact };
        let blocking = track_scrapper_v2::durable::blocking::BlockingStages::new();
        let workspace = Workspace::open(&root, operation_id, &spec)?;
        operations::execute_import(&workspace, &spec, &test.url, &blocking).await
    }

    async fn manifest(&self, root: &Path, test: &Scenario) -> Result<Value> {
        let (profile_id, athlete_id, school_id): (i64, i64, i64) = sqlx::query_as(
            "SELECT id,athlete_id,school_id FROM athlete_sources WHERE source='milesplit' AND source_id=$1",
        )
        .bind(PROFILE_ID)
        .fetch_one(&test.pool)
        .await?;
        let key: String =
            sqlx::query_scalar("SELECT source_result_key FROM results WHERE source_result_id=$1")
                .bind(&self.result_id)
                .fetch_one(&test.pool)
                .await?;
        let url = profile_url(PROFILE_ID);
        let result_capture = stage(root, "result.txt", self.published.as_bytes())?;
        let profile_capture = stage(
            root,
            "profile.txt",
            b"Casey Runner | Fall Creek | Class of 2027",
        )?;
        Ok(json!({
            "schema_revision":1,"reviewed_by":"codex","researcher":"gpt-6-luna",
            "authorization_quote":"Please link the verified result to this fixture profile.",
            "reviewed_at":"2026-10-30T14:00:00Z",
            "evidence":[
                {"artifact":result_capture,"primary_url":RESULT_URL,"captured_at":"2026-10-30T13:00:00Z","capture_kind":"retained_html"},
                {"artifact":profile_capture,"primary_url":url,"captured_at":"2026-10-30T13:01:00Z","capture_kind":"web_tool_text"}
            ],
            "changes":[{
                "source":"milesplit","source_result_key":key,"source_result_id":self.result_id,
                "source_url":RESULT_URL,"event":RESULT_EVENT,"mark":self.mark,"sport":"cross_country",
                "meet_id":MEET_ID,"meet_date":MEET_DATE,"status":"recorded","represented_school_id":null,
                "raw_name":PROFILE_NAME,"raw_team":SCHOOL,"raw_grade":"SR","published_file_identity":FILE_IDENTITY,
                "line_ordinal":self.ordinal,"published_html":self.published,"target_source_id":PROFILE_ID,
                "target_athlete_source_id":profile_id,"target_athlete_id":athlete_id,"target_profile_url":url,
                "target_name":PROFILE_NAME,"target_gender":"F","target_graduation_year":2027,
                "target_school_id":school_id,"target_school_name":SCHOOL,"target_school_state":"WI",
                "result_evidence":[0],"identity_evidence":[1]
            }]
        }))
    }

    pub async fn assert_exported(&self, database: &Database) -> Result<()> {
        let (records, _) = database
            .export_snapshot(&["WI".to_owned()], &[2027])
            .await?;
        let record = records
            .iter()
            .find(|record| record.name == PROFILE_NAME)
            .context("canonical profile export")?;
        let exported = record
            .performances
            .iter()
            .filter_map(|entry| {
                (entry.performance.result_id.as_deref() == Some(self.result_id.as_str()))
                    .then_some(entry.performance.mark.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(exported, vec![self.mark]);
        Ok(())
    }
}

fn prior_owned() -> Performance {
    Performance {
        athlete_id: PROFILE_ID.to_owned(),
        event: RESULT_EVENT.to_owned(),
        mark: "23:21.5".to_owned(),
        reported_status: None,
        source_values: Some(json!({"kind":"raw_result","published_file_identity":FILE_IDENTITY})),
        sport: Sport::CrossCountry,
        result_id: Some(PRIOR_OWNED_ID.to_owned()),
        source_row: None,
        school_id: Some(SCHOOL_ID.to_owned()),
        meet_id: Some(MEET_ID.to_owned()),
        meet_name: Some("Fall Creek Invitational".to_owned()),
        date: Some(MEET_DATE.to_owned()),
        wind: None,
        timing: None,
        course: None,
        is_personal_record: false,
        is_relay: false,
        season: None,
        grade: None,
        gender: Some("F".to_owned()),
    }
}

fn stage(root: &Path, name: &str, bytes: &[u8]) -> Result<Artifact> {
    let path = root.join(name);
    std::fs::write(&path, bytes)?;
    operations::stage_input(root, &path)
}
