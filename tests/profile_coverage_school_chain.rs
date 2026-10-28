mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, Job},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
};

async fn school(test: &support::Scenario, id: &str, athlete_id: &str) -> Result<Target> {
    let school: i64 = sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit',$1,'Published School','WI',$2) RETURNING id")
        .bind(id).bind(format!("https://wi.milesplit.com/teams/{id}")).fetch_one(&test.pool).await?;
    let athlete: i64 = sqlx::query_scalar("INSERT INTO athletes(name,graduation_year,school_id,state) VALUES('Published Runner',2027,$1,'WI') RETURNING id")
        .bind(school).fetch_one(&test.pool).await?;
    let target = Target::get(
        Source::MileSplit,
        PageKind::Athlete,
        format!("https://wi.milesplit.com/athletes/{athlete_id}-runner"),
        PageContext {
            school_id: Some(id.into()),
            athlete_id: Some(athlete_id.into()),
            state: Some("WI".into()),
            ..PageContext::default()
        },
    );
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,school_id,graduation_year) VALUES($1,'milesplit',$2,$3,$4,2027)")
        .bind(athlete).bind(athlete_id).bind(&target.url).bind(school).execute(&test.pool).await?;
    Ok(target)
}

fn parent(profile: &Target, kind: PageKind) -> Result<Target> {
    let id = profile
        .context
        .school_id
        .as_deref()
        .context("school source ID")?;
    let url = match kind {
        PageKind::Team => format!("https://wi.milesplit.com/teams/{id}-published"),
        PageKind::Roster => format!("https://wi.milesplit.com/teams/{id}-published/roster"),
        PageKind::Contacts => format!("https://school{id}.example.edu/staff"),
        _ => anyhow::bail!("invalid school parent fixture"),
    };
    let mut context = profile.context.clone();
    context.athlete_id = None;
    Ok(Target::get(Source::MileSplit, kind, url, context))
}

fn followup(profile: &Target, suffix: &str) -> Target {
    let mut target = profile.clone();
    target.url.push('/');
    target.url.push_str(suffix);
    target
}

async fn next(database: &Database, contacts: bool) -> Result<Job> {
    database
        .next_profile_coverage_job(
            Some(Source::MileSplit),
            3,
            &["WI".into()],
            &[2027],
            contacts,
        )
        .await?
        .context("next school chain stage")
}

async fn finish(database: &Database, expected: &Target, contacts: bool) -> Result<()> {
    let job = next(database, contacts).await?;
    assert_eq!(job.target.url, expected.url);
    assert_eq!(job.target.kind, expected.kind);
    assert!(database.start_attempt(job.id, 3).await?);
    database
        .persist(
            Some(job.id),
            &job.target,
            &Capture {
                url: job.target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: b"<html></html>".to_vec(),
            },
            &ParsedPage::default(),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn given_interleaved_school_parents_when_resuming_after_first_team_then_roster_each_athlete_results_and_contacts_finish_before_next_school()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let first = school(test,"101","81").await?;
        let second = school(test,"102","82").await?;
        let first_team = parent(&first,PageKind::Team)?;
        let second_team = parent(&second,PageKind::Team)?;
        let roster = parent(&first,PageKind::Roster)?;
        let contact = parent(&first,PageKind::Contacts)?;
        let stats = followup(&first,"stats"); let progression = followup(&first,"progression");
        let mut team_alias = first_team.clone(); team_alias.url="https://wi.milesplit.com/teams/101".into();
        let mut stats_duplicate = stats.clone(); stats_duplicate.context.sport=Some(Sport::Outdoor);
        database.enqueue(&[first_team.clone(),second_team.clone(),first.clone(),second,roster.clone(),stats.clone(),progression.clone(),contact.clone(),team_alias.clone(),stats_duplicate.clone()],false).await?;
        finish(&database,&first_team,true).await?;
        database.close().await;
        let database = Database::connect(&test.url).await?;
        for expected in [&roster,&first,&stats,&progression] { finish(&database,expected,true).await?; }
        assert_eq!(next(&database,false).await?.target.url,second_team.url);
        finish(&database,&contact,true).await?;
        assert_eq!(next(&database,true).await?.target.url,second_team.url);
        let skipped: Vec<(String,i32)> = sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE url=$1 OR (url=$2 AND cursor->'context'->>'sport'='outdoor') ORDER BY id")
            .bind(&team_alias.url).bind(&stats_duplicate.url).fetch_all(&test.pool).await?;
        assert_eq!(skipped,vec![("pending".into(),0),("pending".into(),0)]);
        database.close().await; Ok(())
    })).await
}

#[tokio::test]
async fn given_exhausted_first_team_when_other_school_is_unattempted_then_failure_is_not_hidden_by_new_work()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let first=school(test,"201","91").await?; let second=school(test,"202","92").await?;
        let first_team=parent(&first,PageKind::Team)?; let second_team=parent(&second,PageKind::Team)?;
        database.enqueue(&[first_team.clone(),second_team,first,second],false).await?;
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='retained parent failure' WHERE url=$1").bind(&first_team.url).execute(&test.pool).await?;
        assert!(database.next_profile_coverage_job(Some(Source::MileSplit),3,&["WI".into()],&[2027],true).await?.is_none());
        assert!(database.check_profile_coverage_exhausted(Source::MileSplit,3,&["WI".into()],&[2027],true).await.is_err());
        let state: (String,i32)=sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE url=$1").bind(&first_team.url).fetch_one(&test.pool).await?;
        assert_eq!(state,("failed".into(),3));
        database.close().await; Ok(())
    })).await
}

#[tokio::test]
async fn given_queued_team_with_no_known_cohort_when_school_chain_runs_then_team_and_roster_discover_it_before_next_school()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let first=school(test,"301","101").await?;
        sqlx::query("DELETE FROM athlete_sources WHERE source_id='101'").execute(&test.pool).await?;
        sqlx::query("DELETE FROM athletes WHERE school_id=(SELECT id FROM schools WHERE source_id='301')").execute(&test.pool).await?;
        sqlx::query("DELETE FROM schools WHERE source_id='301'").execute(&test.pool).await?;
        let second=school(test,"302","102").await?;
        let first_team=parent(&first,PageKind::Team)?; let roster=parent(&first,PageKind::Roster)?;
        let second_team=parent(&second,PageKind::Team)?;
        database.enqueue(&[first_team.clone(),second_team.clone(),roster.clone()],false).await?;
        finish(&database,&first_team,false).await?;
        finish(&database,&roster,false).await?;
        let discovered=school(test,"301","101").await?;
        assert_eq!(discovered.url,first.url);
        database.enqueue(std::slice::from_ref(&first),false).await?;
        finish(&database,&first,false).await?;
        assert_eq!(next(&database,false).await?.target.url,second_team.url);
        database.close().await; Ok(())
    })).await
}

#[tokio::test]
async fn given_exhausted_optional_contact_when_next_school_is_ready_then_contact_failure_is_preserved_and_next_team_runs()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let first=school(test,"401","111").await?;
        let second=school(test,"402","112").await?;
        let first_team=parent(&first,PageKind::Team)?;
        let second_team=parent(&second,PageKind::Team)?;
        let contact=parent(&first,PageKind::Contacts)?;
        database.enqueue(&[first_team.clone(),second_team.clone(),first.clone(),contact.clone()],false).await?;
        finish(&database,&first_team,true).await?;
        finish(&database,&first,true).await?;
        sqlx::query("UPDATE scrape_pages SET status='failed',attempts=3,last_error='navigating browser page: Request timed out.',notes=ARRAY['Original contact acquisition failure retained'] WHERE url=$1")
            .bind(&contact.url).execute(&test.pool).await?;
        let before: serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE url=$1").bind(&contact.url).fetch_one(&test.pool).await?;
        assert_eq!(next(&database,true).await?.target.url,second_team.url);
        database.check_profile_coverage_exhausted(Source::MileSplit,3,&["WI".into()],&[2027],true).await?;
        let after: serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE url=$1").bind(&contact.url).fetch_one(&test.pool).await?;
        assert_eq!(after,before);
        finish(&database,&second_team,true).await?;
        assert!(database.next_profile_coverage_job(Some(Source::MileSplit),3,&["WI".into()],&[2027],true).await?.is_none());
        database.check_profile_coverage_exhausted(Source::MileSplit,3,&["WI".into()],&[2027],true).await?;
        database.close().await; Ok(())
    })).await
}
