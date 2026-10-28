mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    model::{PageContext, PageKind, Source, Target},
    sources,
};

const REVIEWED: &str = "https://mn.milesplit.com/athletes/13557507-tana-borland/photos";

fn config(review: &str) -> String {
    format!(
        "states=['WI']\ngraduation_years=[2027]\nseason=2026\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\nenabled=true\ntransport='http'\n[acquisition]\nmode='public_results'\nreviewed_media_stops=['{review}']\n"
    )
}

fn page(url: &str, kind: PageKind) -> Target {
    Target::get(
        Source::MileSplit,
        kind,
        url.into(),
        PageContext {
            state: Some("WI".into()),
            school_id: (kind == PageKind::Contacts).then(|| "1".into()),
            ..PageContext::default()
        },
    )
}

async fn seed(database: &Database) -> Result<()> {
    database
        .enqueue(&sources::milesplit::seeds(&["WI".into()], 2026)?, false)
        .await?;
    sqlx::query("UPDATE scrape_pages SET status='succeeded'")
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn scrape(test: &support::Scenario) -> Result<std::process::Output> {
    let path = test.directory().join("public-results.toml");
    std::fs::write(&path, config(REVIEWED))?;
    test.cli(&[
        "scrape",
        "--config",
        path.to_str().context("UTF-8 config path")?,
    ])
    .await
}

#[test]
fn given_non_media_or_ambiguous_review_when_configured_then_review_is_rejected() {
    assert!(Config::parse(&config(REVIEWED)).is_ok());
    for url in [
        "https://wi.milesplit.com/athletes/1",
        "https://evil.example/athletes/1/photos",
        "http://wi.milesplit.com/athletes/1/photos",
        "https://wi.milesplit.com/athletes/1/%70hotos",
        "https://wi.milesplit.com/athletes/1/photos?allow=1",
        "https://wi.milesplit.com/athletes//1/photos",
        "https://wi.milesplit.com/athletes/1/photos/",
        "https://wi.milesplit.com/athletes/1/photos#review",
        "https://user:password@wi.milesplit.com/athletes/1/photos",
    ] {
        assert!(Config::parse(&config(url)).is_err(), "{url}");
    }
}

#[tokio::test]
async fn given_reviewed_photo_stop_when_public_work_is_scoped_then_exclusions_consume_no_attempts()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        seed(&database).await?;
        let prior = page(REVIEWED, PageKind::Athlete);
        database.enqueue(&[prior], false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=1,last_error='reviewed subscription photo stop' WHERE url=$1").bind(REVIEWED).execute(database.pool()).await?;
        let before: (i64, String, i32, String) = sqlx::query_as("SELECT id,status,attempts,last_error FROM scrape_pages WHERE url=$1").bind(REVIEWED).fetch_one(database.pool()).await?;
        let excluded = [page("https://wi.milesplit.com/athletes/1/photos",PageKind::Athlete),page("https://wi.milesplit.com/athletes/1/videos",PageKind::Athlete),page("https://school.example/staff",PageKind::Contacts),page("https://wi.milesplit.com/rankings",PageKind::Directory)];
        database.enqueue(&excluded, false).await?;
        support::assert_success(&scrape(test).await?);
        let after: (i64, String, i32, String) = sqlx::query_as("SELECT id,status,attempts,last_error FROM scrape_pages WHERE url=$1").bind(REVIEWED).fetch_one(database.pool()).await?;
        assert_eq!(after,before);
        let states: Vec<(String,i32,String)> = sqlx::query_as("SELECT status,attempts,cursor->>'public_results_exclusion' FROM scrape_pages WHERE url<>$1 AND status='blocked' ORDER BY id").bind(REVIEWED).fetch_all(database.pool()).await?;
        assert_eq!(states,vec![("blocked".into(),0,"public-results/v1".into());4]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_real_public_page_denial_when_public_results_runs_then_all_source_work_stops_unchanged()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        seed(&database).await?;
        let denied = page("https://wi.milesplit.com/athletes/2",PageKind::Athlete);
        database.enqueue(&[denied],false).await?;
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=1,last_error='HTTP 403 access denied',cursor=jsonb_set(cursor,'{public_results_exclusion}','\"public-results/v1\"') WHERE url LIKE '%/athletes/2'").execute(database.pool()).await?;
        let before: Vec<(i64,String,i32,Option<String>)> = sqlx::query_as("SELECT id,status,attempts,last_error FROM scrape_pages ORDER BY id").fetch_all(database.pool()).await?;
        let output = scrape(test).await?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("blocked page requiring human review"));
        let after: Vec<(i64,String,i32,Option<String>)> = sqlx::query_as("SELECT id,status,attempts,last_error FROM scrape_pages ORDER BY id").fetch_all(database.pool()).await?;
        assert_eq!(after,before);
        database.close().await;
        Ok(())
    })).await
}
