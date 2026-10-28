mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, Job},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Target},
    sources,
};

const OFFICIAL: &str = "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1";
const DETAIL: &str = include_str!("../fixtures/contacts/wiaa-abbotsford-detail.html");

fn target(kind: PageKind, url: &str, school: &str, name: &str) -> Target {
    Target::get(
        Source::MileSplit,
        kind,
        url.into(),
        PageContext {
            school_id: Some(school.into()),
            school_name: Some(name.into()),
            state: Some("WI".into()),
            ..PageContext::default()
        },
    )
}

async fn prepare(test: &support::Scenario) -> Result<(Database, Target, Target, Target)> {
    let database = Database::connect(&test.url).await?;
    sqlx::query("INSERT INTO schools(source,source_id,name,state,city,source_url) VALUES('milesplit','52649','Abbotsford','WI','Abbotsford','https://wi.milesplit.com/teams/52649-abbotsford')").execute(&test.pool).await?;
    let team = target(
        PageKind::Team,
        "https://wi.milesplit.com/teams/52649-abbotsford",
        "52649",
        "Abbotsford",
    );
    database
        .persist(
            None,
            &team,
            &Capture {
                url: team.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: b"<html></html>".to_vec(),
            },
            &ParsedPage::default(),
        )
        .await?;
    let next = target(
        PageKind::Team,
        "https://wi.milesplit.com/teams/14035-algoma",
        "14035",
        "Algoma",
    );
    let web = target(
        PageKind::Contacts,
        "https://abbotsford.example.edu/staff",
        "52649",
        "Abbotsford",
    );
    let official = target(PageKind::Contacts, OFFICIAL, "52649", "Abbotsford");
    database
        .enqueue(&[next.clone(), web.clone(), official.clone()], false)
        .await?;
    Ok((database, web, official, next))
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
        .context("school chain job")
}

async fn official(database: &Database, target: &Target) -> Result<()> {
    let job = next(database, true).await?;
    assert_eq!(job.target.url, target.url);
    let parsed = sources::wiaa::parse(&job.target, DETAIL)?;
    assert_eq!(parsed.coaches.len(), 4);
    assert!(
        parsed
            .coaches
            .iter()
            .all(|c| c.name.is_some() && c.email.is_some())
    );
    assert_eq!(
        parsed.schools[0].address.as_deref(),
        Some("510 West Hemlock St")
    );
    assert_eq!(parsed.schools[0].postal_code.as_deref(), Some("54405-9730"));
    assert!(database.start_attempt(job.id, 3).await?);
    database
        .persist(
            Some(job.id),
            &job.target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: DETAIL.as_bytes().to_vec(),
            },
            &parsed,
        )
        .await?;
    Ok(())
}

async fn contacts(test: &support::Scenario) -> Result<Vec<serde_json::Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(c) FROM contacts c ORDER BY id")
            .fetch_all(&test.pool)
            .await?,
    )
}

#[tokio::test]
async fn given_later_wiaa_contact_when_school_fields_are_fulfilled_then_official_runs_first_and_old_website_is_deferred_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let (database, web, wiaa, next_team) = prepare(test).await?;
            let before: serde_json::Value =
                sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE url=$1")
                    .bind(&web.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(next(&database, false).await?.target.url, next_team.url);
            official(&database, &wiaa).await?;
            let saved = contacts(test).await?;
            assert_eq!(saved.len(), 4);
            assert_eq!(next(&database, true).await?.target.url, next_team.url);
            let after: serde_json::Value =
                sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE url=$1")
                    .bind(&web.url)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(after, before);
            assert_eq!(contacts(test).await?, saved);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_missing_email_or_postal_address_when_official_page_succeeded_then_legacy_website_remains_eligible()
-> Result<()> {
    for modification in [
        "UPDATE contacts SET email=NULL",
        "UPDATE contacts SET email=NULL WHERE role LIKE '%Girls Track%'",
        "DELETE FROM contacts WHERE role LIKE '%Girls Track%'",
        "UPDATE schools SET address=NULL",
        "UPDATE schools SET postal_code=NULL",
    ] {
        support::with_schema(|test| {
            Box::pin(async move {
                let (database, web, wiaa, _) = prepare(test).await?;
                official(&database, &wiaa).await?;
                sqlx::query(sqlx::AssertSqlSafe(modification))
                    .execute(&test.pool)
                    .await?;
                let saved = contacts(test).await?;
                assert_eq!(next(&database, true).await?.target.url, web.url);
                assert_eq!(contacts(test).await?, saved);
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_stale_other_source_or_unnamed_coach_claim_when_official_checkpoint_exists_then_legacy_website_is_not_deferred()
-> Result<()> {
    for modification in [
        "DELETE FROM contacts",
        "UPDATE contacts SET source_url='https://other-source.example.edu/staff'",
        "UPDATE contacts SET name=NULL",
        "UPDATE contacts SET raw=jsonb_set(raw,'{review_notes}','[\"Earlier contact source claim; not confirmed by latest parse\"]'::jsonb)",
        "UPDATE scrape_pages SET status='failed',attempts=3 WHERE url='https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1'",
    ] {
        support::with_schema(|test| {
            Box::pin(async move {
                let (database, web, wiaa, _) = prepare(test).await?;
                official(&database, &wiaa).await?;
                sqlx::query(sqlx::AssertSqlSafe(modification))
                    .execute(&test.pool)
                    .await?;
                assert_eq!(next(&database, true).await?.target.url, web.url);
                database.close().await;
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_official_coaches_attached_to_another_school_when_address_is_complete_then_they_do_not_satisfy_this_school()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        let (database,web,wiaa,_)=prepare(test).await?;
        official(&database,&wiaa).await?;
        let other:i64=sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','999','Another School','WI','https://wi.milesplit.com/teams/999') RETURNING id").fetch_one(&test.pool).await?;
        sqlx::query("UPDATE contacts SET school_id=$1").bind(other).execute(&test.pool).await?;
        assert_eq!(next(&database,true).await?.target.url,web.url);
        database.close().await;Ok(())
    })).await
}
