use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    export::ExportLayout,
    model::{PageContext, ParsedPage, School, Source, Sport, Target},
    sources,
};

#[path = "../support/records.rs"]
mod records;

pub fn config() -> Config {
    let mut config = Config::default();
    config.states = vec!["WI".into()];
    config.sources.athleticnet.enabled = false;
    config.export.layout = ExportLayout::SingleSheet;
    config.export.review_sheet = true;
    config
}

pub fn athlete(id: &str, state: Option<&str>) -> Result<Target> {
    sources::milesplit::athlete_target(id, state, Sport::Outdoor)
}

pub async fn failure(
    database: &Database,
    target: &Target,
    status: &str,
    attempts: i32,
    error: &str,
) -> Result<i64> {
    database
        .enqueue(std::slice::from_ref(target), false)
        .await?;
    sqlx::query_scalar("UPDATE scrape_pages SET status=$2,attempts=$3,last_error=$4,created_at='2026-10-29T12:00:00Z',last_success=NULL WHERE url=$1 RETURNING id")
        .bind(&target.url).bind(status).bind(attempts).bind(error).fetch_one(database.pool()).await.context("seeding review checkpoint")
}

pub async fn seed(database: &Database) -> Result<Vec<i64>> {
    let mut athlete = records::athlete("11");
    athlete.school_id = Some("1000".into());
    records::persist(
        database,
        &records::target(Source::MileSplit, "11"),
        &ParsedPage {
            athletes: vec![athlete],
            performances: vec![records::performance("11.00")],
            schools: vec![
                School {
                    id: "1000".into(),
                    name: "Published School".into(),
                    state: Some("WI".into()),
                    ..School::default()
                },
                School {
                    id: "9000".into(),
                    name: "Other School".into(),
                    state: Some("OH".into()),
                    ..School::default()
                },
            ],
            ..ParsedPage::default()
        },
    )
    .await?;
    let mut first = self::athlete("11", Some("WI"))?;
    first.context.school_id = Some("1000".into());
    let first = failure(database, &first, "failed", 3, "DNS resolution failed").await?;
    let blocked = failure(
        database,
        &self::athlete("12", Some("WI"))?,
        "blocked",
        0,
        "Challenge requires human handling",
    )
    .await?;
    let deferred = failure(
        database,
        &self::athlete("13", Some("WI"))?,
        "failed",
        3,
        "Connection timed out",
    )
    .await?;
    sqlx::query("INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts,retry_at_ms) VALUES(repeat('a',64),$1,'review-fixture','{}','deferred',3,1793278800000)")
        .bind(deferred).execute(database.pool()).await?;
    failure(
        database,
        &self::athlete("14", Some("OH"))?,
        "failed",
        1,
        "Unrelated state",
    )
    .await?;
    let mut other = self::athlete("15", None)?;
    other.context.school_id = Some("9000".into());
    failure(database, &other, "blocked", 1, "Unrelated linked school").await?;
    failure(
        database,
        &self::athlete("16", Some("WI"))?,
        "pending",
        0,
        "Pending is not a failure",
    )
    .await?;
    let root = Target::get(
        Source::AthleticNet,
        track_scrapper_v2::model::PageKind::Directory,
        "https://www.athletic.net/api/v1/public/GetStatesCountries2".into(),
        PageContext::default(),
    );
    let root = failure(database, &root, "failed", 1, "Root request failed").await?;
    let disabled = sources::athleticnet::athlete_target("18", Some("WI"), Sport::Outdoor)?;
    let disabled = failure(
        database,
        &disabled,
        "blocked",
        0,
        "Provider disabled; existing stop retained",
    )
    .await?;
    Ok(vec![first, blocked, deferred, root, disabled])
}
