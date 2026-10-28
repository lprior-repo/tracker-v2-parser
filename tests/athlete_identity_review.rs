#[path = "support/identity_review.rs"]
mod identity;
mod support;

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::Value;
use track_scrapper_v2::{
    database::Database,
    model::{PageContext, PageKind, Source, Target},
    sources,
};

async fn preserved_stop(test: &support::Scenario) -> Result<()> {
    let database = Database::connect(&test.url).await?;
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Athlete,
        "https://www.athletic.net/athlete/99999999".into(),
        PageContext::default(),
    );
    database.enqueue(&[target], false).await?;
    sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=2,last_error='preserved access stop' WHERE url='https://www.athletic.net/athlete/99999999'")
        .execute(&test.pool).await?;
    database.close().await;
    Ok(())
}

async fn assert_workbook(test: &support::Scenario, canonical: i64) -> Result<()> {
    let config = identity::config(test)?;
    let output = test.directory().join("reviewed.xlsx");
    support::assert_success(
        &test
            .cli(&[
                "export",
                "--config",
                config.to_str().context("config")?,
                "--output",
                output.to_str().context("output")?,
            ])
            .await?,
    );
    let mut workbook = open_workbook_auto(&output)?;
    assert_eq!(workbook.sheet_names(), vec!["Athletes"]);
    let cells = workbook.worksheet_range("Athletes")?;
    let rows = cells.rows().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    let header = rows[0].iter().map(ToString::to_string).collect::<Vec<_>>();
    let row = rows[1].iter().map(ToString::to_string).collect::<Vec<_>>();
    let field = |name: &str| -> Result<&str> {
        Ok(row[header
            .iter()
            .position(|h| h == name)
            .context("field header")?]
        .as_str())
    };
    assert_eq!(field("Name")?, "Truman Lefevre");
    assert_eq!(field("Athlete ID")?, canonical.to_string());
    assert!(field("Profile")?.contains(&identity::url(0)));
    assert!(field("Profile")?.contains(&identity::url(1)));
    Ok(())
}

#[tokio::test]
async fn given_user_reviewed_same_provider_profiles_when_linked_and_replayed_then_one_person_keeps_both_profiles_and_all_twelve_results()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        identity::seed(test).await?;
        preserved_stop(test).await?;
        let manifest = identity::manifest(test).await?;
        let canonical = manifest["canonical_athlete_id"].as_i64().context("canonical")?;
        let queue = identity::queue(test).await?;
        let results = identity::results(test).await?;
        let profiles = identity::profiles(test).await?;
        support::assert_success(&identity::apply(test,&manifest).await?);
        assert_eq!(identity::queue(test).await?,queue);
        assert_eq!(identity::results(test).await?,results);
        assert_eq!(identity::profiles(test).await?,profiles);
        let audit: Value = sqlx::query_scalar("SELECT details->'_reviewed_identity' FROM athlete_sources ORDER BY id LIMIT 1").fetch_one(&test.pool).await?;
        assert_eq!(audit["retired_canonical_ids"].as_array().context("retired")?.len(),1);
        assert_eq!(audit["original_profiles"].as_array().context("original profiles")?.len(),2);
        assert_eq!(audit["fact_warnings"].as_array().context("fact warnings")?.len(),2);
        let repeat = identity::apply(test,&manifest).await?;
        support::assert_success(&repeat);
        assert_eq!(serde_json::from_slice::<Value>(&repeat.stdout)?["already_applied"],true);
        for _ in 0..2 { for index in 0..2 {
            identity::replay(test,index,"base").await?;
            identity::replay(test,index,"progression").await?;
        }}
        assert_eq!(identity::canonical_count(test).await?,1);
        assert_eq!(identity::results(test).await?,results);
        let reports = identity::records(test).await?;
        assert_eq!(reports.len(),1);
        assert_eq!(reports[0].id,canonical);
        assert_eq!(reports[0].name,"Truman Lefevre");
        assert!(reports[0].needs_review);
        assert_eq!(reports[0].profiles.len(),2);
        assert_eq!(reports[0].profile_details.len(),2);
        assert_eq!(reports[0].performances.len(),12);
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM athlete_sources ORDER BY id").fetch_all(&test.pool).await?;
        assert_eq!(names,vec!["Truman LeFerve","Truman Lefevre"]);
        let latest: Vec<String> = sqlx::query_scalar("SELECT details->>'gender' FROM athlete_sources ORDER BY id").fetch_all(&test.pool).await?;
        assert_eq!(latest,vec!["female","female"]);
        assert_workbook(test,canonical).await?;
        let stop: (String,i32,String) = sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE url='https://www.athletic.net/athlete/99999999'").fetch_one(&test.pool).await?;
        assert_eq!(stop,("blocked".into(),2,"preserved access stop".into()));
        Ok(())
    })).await
}

#[tokio::test]
async fn given_approved_pair_when_later_hard_facts_disagree_then_identity_stays_linked_and_conflicts_are_audited()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        identity::seed(test).await?;
        let manifest = identity::manifest(test).await?;
        support::assert_success(&identity::apply(test,&manifest).await?);
        let database = Database::connect(&test.url).await?;
        for (field,value) in [("gender","female"),("state","MI"),("year","2028"),("school","14217") ] {
            let body = std::fs::read_to_string(support::fixture("identity/truman-14441466-base.html"))?;
            let target = Target::get(Source::MileSplit,PageKind::Athlete,identity::url(0),PageContext::default());
            let mut page = sources::parse(&target,&body)?;
            let person = page.athletes.first_mut().context("athlete")?;
            match field {
                "gender" => person.gender=Some(value.into()),
                "state" => person.state=Some(value.into()),
                "year" => person.graduation_year=Some(value.parse()?),
                "school" => { person.school_id=Some(value.into()); page.schools.push(track_scrapper_v2::model::School {id:value.into(),name:"Another School".into(),state:Some("WI".into()),..Default::default()}); },
                _ => unreachable!(),
            }
            let capture = track_scrapper_v2::model::Capture {url:target.url.clone(),status:200,content_type:"text/html".into(),body:body.into_bytes()};
            database.persist(None,&target,&capture,&page).await?;
            assert_eq!(identity::canonical_count(test).await?,1);
            let warning: Value = sqlx::query_scalar("SELECT details#>'{_reviewed_identity,latest_fact_conflicts}' FROM athlete_sources ORDER BY id LIMIT 1").fetch_one(&test.pool).await?;
            assert_eq!(warning["milesplit:14441466"][match field {"year"=>"graduation_year","school"=>"school_id",_=>field}], if field=="year" {serde_json::json!(2028)} else {serde_json::json!(value)});
        }
        assert!(identity::records(test).await?[0].needs_review);
        database.close().await;
        Ok(())
    })).await
}
