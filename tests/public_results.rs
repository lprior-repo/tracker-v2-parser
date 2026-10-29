#[path = "durable_page/support.rs"]
mod page_support;
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
        let excluded = [page("https://wi.milesplit.com/athletes/1/photos",PageKind::Athlete),page("https://wi.milesplit.com/athletes/1/videos",PageKind::Athlete),page("https://school.example/staff",PageKind::Contacts)];
        database.enqueue(&excluded, false).await?;
        let config=Config::parse(&config(REVIEWED))?;
        track_scrapper_v2::runner::durable::bootstrap(&database,&config).await?;
        let scope=page_support::scope(&config)?;
        assert!(track_scrapper_v2::durable::page::select(&database,&config,&scope,chrono::Utc::now().timestamp_millis()).await?.is_none());
        let mut transport=track_scrapper_v2::transport::Transport::new(track_scrapper_v2::transport::Options {mode:track_scrapper_v2::transport::Mode::Http,driver_url:None,profile:test.directory().join("browser"),headless:true,rps:1.0})?;
        let refused=transport.fetch(&page("https://wi.milesplit.com/rankings",PageKind::Directory)).await.err().context("robots-restricted route")?;
        assert!(refused.blocked && refused.capture.is_none());assert!(refused.detail.contains("robots"));
        transport.close().await?;
        let after: (i64, String, i32, String) = sqlx::query_as("SELECT id,status,attempts,last_error FROM scrape_pages WHERE url=$1").bind(REVIEWED).fetch_one(database.pool()).await?;
        assert_eq!(after,before);
        let states: Vec<(String,i32)> = sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE url<>$1 AND status='pending' ORDER BY id").bind(REVIEWED).fetch_all(database.pool()).await?;
        assert_eq!(states,vec![("pending".into(),0);3]);
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
        let error=track_scrapper_v2::runner::durable::bootstrap(&database,&Config::parse(&config(REVIEWED))?).await.err().context("preserved source access stop")?;
        assert!(format!("{error:#}").contains("blocked page requiring human review"));
        let after: Vec<(i64,String,i32,Option<String>)> = sqlx::query_as("SELECT id,status,attempts,last_error FROM scrape_pages ORDER BY id").fetch_all(database.pool()).await?;
        assert_eq!(after,before);
        database.close().await;
        Ok(())
    })).await
}
