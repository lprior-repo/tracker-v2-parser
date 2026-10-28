mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, Job},
    model::{Capture, PageContext, PageKind, Source, Target},
    sources,
};

async fn unknown(test: &support::Scenario, id: &str) -> Result<Target> {
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://wi.milesplit.com/athletes/{id}-runner"),
        PageContext {
            athlete_id: Some(id.into()),
            state: Some("WI".into()),
            ..PageContext::default()
        },
    );
    let athlete: i64 = sqlx::query_scalar(
        "INSERT INTO athletes(name,state) VALUES('Published Runner','WI') RETURNING id",
    )
    .fetch_one(&test.pool)
    .await?;
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url) VALUES($1,'milesplit',$2,$3)").bind(athlete).bind(id).bind(&target.url).execute(&test.pool).await?;
    Ok(target)
}

async fn next(database: &Database) -> Result<Option<Job>> {
    database
        .next_profile_coverage_job(Some(Source::MileSplit), 3, &["WI".into()], &[2027], false)
        .await
}

async fn capture_year(database: &Database, target: &Target, year: Option<i32>) -> Result<()> {
    let job = next(database)
        .await?
        .context("unknown-year base must be fetched")?;
    assert_eq!(job.target.url, target.url);
    let graduation = year.map_or_else(String::new, |year| {
        format!("<span class='grad-year'>Class of {year}</span>")
    });
    let html = format!(
        "<link rel='canonical' href='{}'><h1 id='athleteName'>Published Runner</h1><div class='athlete-info'>{graduation}</div><div class='profile-navigation'><a href='{}/stats'>Stats</a><a href='{}/progression'>Progression</a></div>",
        target.url, target.url, target.url
    );
    let parsed = sources::parse(&job.target, &html)?;
    assert_eq!(parsed.athletes[0].graduation_year, year);
    assert!(database.start_attempt(job.id, 3).await?);
    database
        .persist(
            Some(job.id),
            &job.target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: html.into_bytes(),
            },
            &parsed,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn given_unknown_year_roster_profile_when_base_confirms_selected_year_then_saved_stats_and_progression_become_eligible()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let target=unknown(test,"801").await?;
        database.enqueue(std::slice::from_ref(&target),false).await?;
        capture_year(&database,&target,Some(2027)).await?;
        for suffix in ["stats","progression"] {
            let job=next(&database).await?.context("newly confirmed cohort result followup")?;
            assert_eq!(job.target.url,format!("{}/{suffix}",target.url));
            assert!(database.start_attempt(job.id,3).await?);
            let body=format!("<link rel='canonical' href='{}'><h1 id='athleteName'>Published Runner</h1><div class='athlete-info'><span class='grad-year'>Class of 2027</span></div><div class='profile-navigation'></div>",target.url);
            let parsed=sources::parse(&job.target,&body)?;
            database.persist(Some(job.id),&job.target,&Capture {url:job.target.url.clone(),status:200,content_type:"text/html".into(),body:body.into_bytes()},&parsed).await?;
        }
        assert!(next(&database).await?.is_none());
        let year:Option<i32>=sqlx::query_scalar("SELECT graduation_year FROM athletes").fetch_one(&test.pool).await?;
        assert_eq!(year,Some(2027));
        database.close().await;Ok(())
    })).await
}

#[tokio::test]
async fn given_unknown_year_profiles_when_base_confirms_other_year_or_remains_unknown_then_saved_followups_stay_unattempted()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        for (id,year) in [("802",Some(2028)),("803",None)] {
            let target=unknown(test,id).await?;
            database.enqueue(std::slice::from_ref(&target),false).await?;
            capture_year(&database,&target,year).await?;
            assert!(next(&database).await?.is_none());
            let preserved:Vec<(String,i32)>=sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE cursor->'context'->>'athlete_id'=$1 AND url<>$2 ORDER BY id").bind(id).bind(&target.url).fetch_all(&test.pool).await?;
            assert_eq!(preserved,vec![("pending".into(),0),("pending".into(),0)]);
        }
        database.close().await;Ok(())
    })).await
}
