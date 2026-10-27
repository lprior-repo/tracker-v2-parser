mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    database::{Database, JobClass},
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Sport, Target},
    runner::Runner,
    sources,
};

fn school(id: &str, name: &str, website: &str) -> School {
    School {
        id: id.into(),
        name: name.into(),
        state: Some("CA".into()),
        website: Some(website.into()),
        ..School::default()
    }
}

fn athletics_target() -> Result<Target> {
    sources::milesplit::team_target("1234", Some("CA"), Sport::Outdoor, 2025)
}

fn athletics_body(website: &str) -> String {
    format!(
        "<html><head><link rel='canonical' href='https://ca.milesplit.com/teams/1234'></head><body><div class='jumbotron-content'><h1>Example High School</h1><div class='profile'><ul class='info'><li><span>Example City, CA 90210</span><a href='{website}'>Website</a></li></ul></div></div><a class='navigation-link' href='/teams/1234/roster'>Roster</a></body></html>"
    )
}

async fn save(
    database: &Database,
    id: Option<i64>,
    target: &Target,
    body: &str,
    page: &ParsedPage,
) -> Result<()> {
    database
        .persist(
            id,
            target,
            &Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: body.as_bytes().to_vec(),
            },
            page,
        )
        .await?;
    Ok(())
}

async fn insert_school(database: &Database, school: School) -> Result<()> {
    let target = athletics_target()?;
    save(
        database,
        None,
        &target,
        "school observation",
        &ParsedPage {
            schools: vec![school],
            ..ParsedPage::default()
        },
    )
    .await
}

async fn checkpoint_rows(database: &Database) -> Result<Vec<(String, String, i32)>> {
    Ok(sqlx::query_as("SELECT cursor->'context'->>'school_id',status,attempts FROM scrape_pages WHERE cursor->>'kind'='contacts' ORDER BY cursor->'context'->>'school_id'")
        .fetch_all(database.pool()).await?)
}

#[tokio::test]
async fn given_persisted_websites_when_backfilled_and_restarted_then_source_ids_and_checkpoint_states_are_stable()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        for id in ["701", "702", "703", "704"] {
            insert_school(&database, school(id, "Example High School", &format!("https://school{id}.example.edu/"))).await?;
        }
        insert_school(&database, school("705", "Invalid School", "not a URL")).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,website,source_url) VALUES ('nces','nces-1','NCES School','CA','https://nces.example.edu/','nces'),('athleticnet','an-1','Disabled School','CA','https://disabled.example.edu/','athleticnet'),('milesplit','other-state','Other State School','OH','https://other.example.edu/','milesplit')")
            .execute(&test.pool).await?;
        assert_eq!(database.backfill_contacts(&["CA".into()], &[Source::MileSplit], 2026).await?, 4);
        sqlx::query("UPDATE scrape_pages SET status=CASE cursor->'context'->>'school_id' WHEN '701' THEN 'blocked' WHEN '702' THEN 'failed' WHEN '703' THEN 'succeeded' ELSE 'pending' END, attempts=CASE cursor->'context'->>'school_id' WHEN '701' THEN 1 WHEN '702' THEN 3 WHEN '703' THEN 2 ELSE 0 END,last_error='retained diagnostic' WHERE cursor->>'kind'='contacts'")
            .execute(&test.pool).await?;
        let before = checkpoint_rows(&database).await?;
        database.close().await;
        let database = Database::connect(&test.url).await?;
        assert_eq!(database.backfill_contacts(&["CA".into()], &[Source::MileSplit], 2026).await?, 0);
        assert_eq!(database.backfill_contacts(&["CA".into()], &[Source::MileSplit], 2027).await?, 0);
        assert_eq!(checkpoint_rows(&database).await?, before);
        assert_eq!(before, vec![("701".into(),"blocked".into(),1),("702".into(),"failed".into(),3),("703".into(),"succeeded".into(),2),("704".into(),"pending".into(),0)]);
        let contexts: Vec<(String, i32, String)> = sqlx::query_as("SELECT cursor->'context'->>'school_name',(cursor->'context'->>'season')::int,last_error FROM scrape_pages WHERE cursor->>'kind'='contacts'")
            .fetch_all(&test.pool).await?;
        assert!(contexts.iter().all(|row| row == &("Example High School".into(),2026,"retained diagnostic".into())));
        assert_eq!(database.counts().await?.coaches, 0);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_new_school_when_checkpoint_commit_fails_then_school_and_contacts_discovery_roll_back_together()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = athletics_target()?;
        let body = athletics_body("https://school.example.edu/");
        let page = sources::parse(&target, &body)?;
        database.enqueue(std::slice::from_ref(&target), false).await?;
        let job = database.next_job(None, 3, &[]).await?.context("school job")?;
        assert!(database.start_attempt(job.id, 3).await?);
        assert!(save(&database, Some(job.id + 1), &target, &body, &page).await.is_err());
        let counts: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM schools),(SELECT count(*) FROM scrape_pages WHERE cursor->>'kind'='contacts'),(SELECT count(*) FROM scrape_pages WHERE status='succeeded')")
            .fetch_one(&test.pool).await?;
        assert_eq!(counts, (0, 0, 0));
        save(&database, Some(job.id), &target, &body, &page).await?;
        let contact = database.next_job_with_priority(Some(Source::MileSplit), 3, JobClass::Contacts, &[]).await?.context("newly discovered contact")?;
        assert_eq!(contact.target.kind, PageKind::Contacts);
        assert_eq!(contact.target.url, "https://school.example.edu/");
        assert_eq!(contact.target.context.school_id.as_deref(), Some("1234"));
        assert_eq!(contact.target.context.season, Some(2025));
        let binding: (String, String) = sqlx::query_as("SELECT s.source_id,p.status FROM schools s JOIN scrape_pages p ON p.cursor->'context'->>'school_id'=s.source_id WHERE p.cursor->>'kind'='team'")
            .fetch_one(&test.pool).await?;
        assert_eq!(binding, ("1234".into(), "succeeded".into()));
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_invalid_website_or_missing_season_when_school_is_parsed_then_athletics_and_diagnostics_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for (website, season, diagnostic) in [
                ("not-a-url", Some(2025), "invalid official school URL"),
                ("https://school.example.edu/", None, "no season"),
                (
                    "https://school.example.edu:1234/",
                    Some(2025),
                    "unsupported port",
                ),
            ] {
                let mut target = athletics_target()?;
                target.context.season = season;
                let body = athletics_body(website);
                let page = sources::parse(&target, &body)?;
                assert!(page.next.iter().any(|next| next.kind == PageKind::Roster));
                assert!(page.next.iter().all(|next| next.kind != PageKind::Contacts));
                assert!(page.notes.iter().any(|note| note.contains(diagnostic)));
                save(&database, None, &target, &body, &page).await?;
                let observed: String = sqlx::query_scalar(
                    "SELECT website FROM schools WHERE source='milesplit' AND source_id='1234'",
                )
                .fetch_one(&test.pool)
                .await?;
                assert_eq!(observed, website);
            }
            let contacts: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM scrape_pages WHERE cursor->>'kind'='contacts'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(contacts, 0);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_public_miramonte_capture_when_contact_checkpoint_is_saved_then_only_published_coaches_bind_to_source_school()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        insert_school(&database, school("1234", "Miramonte High School", "https://gomats.org/staff-directory")).await?;
        insert_school(&database, school("9876", "Other School", "https://other.example.edu/")).await?;
        database.backfill_contacts(&["CA".into()], &[Source::MileSplit], 2026).await?;
        let job = database.next_job_with_priority(None, 3, JobClass::Contacts, &[]).await?.context("Miramonte contact job")?;
        assert_eq!(job.target.context.school_id.as_deref(), Some("1234"));
        assert!(database.start_attempt(job.id, 3).await?);
        let body = include_str!("../fixtures/contacts/miramonte-staff.html");
        let page = sources::parse(&job.target, body)?;
        save(&database, Some(job.id), &job.target, body, &page).await?;
        let rows: Vec<(String,String,String,String,Option<String>,String)> = sqlx::query_as("SELECT s.source,s.source_id,c.kind,c.email,c.name,c.source_url FROM contacts c JOIN schools s ON s.id=c.school_id ORDER BY c.email")
            .fetch_all(&test.pool).await?;
        assert_eq!(rows, vec![
            ("milesplit".into(),"1234".into(),"coach".into(),"bhenderson@auhsdschools.org".into(),Some("Brian Henderson".into()),job.target.url.clone()),
            ("milesplit".into(),"1234".into(),"coach".into(),"caljumper259@gmail.com".into(),Some("Robert Kennedy".into()),job.target.url.clone()),
            ("milesplit".into(),"1234".into(),"coach".into(),"dharris@auhsdschools.org".into(),Some("Dax Harris".into()),job.target.url.clone()),
        ]);
        let state: (String,i32) = sqlx::query_as("SELECT status,attempts FROM scrape_pages WHERE id=$1").bind(job.id).fetch_one(&test.pool).await?;
        assert_eq!(state, ("succeeded".into(),1));
        assert_eq!(database.enqueue(std::slice::from_ref(&job.target), true).await?, 0);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_scoped_office_fixture_when_persisted_then_office_is_not_a_coach_and_discovery_does_not_reseed_homepage()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let school = school("1234", "Example High School", "https://school.example.edu/athletics/staff");
        insert_school(&database, school.clone()).await?;
        let mut target = sources::contacts::targets(Source::MileSplit, &school, 2026)?
            .into_iter().next().context("official target")?;
        target.context.subject_id = Some("contacts-depth:2".into());
        let body = include_str!("../fixtures/contacts/scoped-school.html");
        let page = sources::parse(&target, body)?;
        assert!(page.next.is_empty());
        save(&database, None, &target, body, &page).await?;
        let office: (String,String,String,String) = sqlx::query_as("SELECT s.source_id,c.kind,c.email,c.source_url FROM contacts c JOIN schools s ON s.id=c.school_id WHERE c.kind='school_office'").fetch_one(&test.pool).await?;
        assert_eq!(office, ("1234".into(),"school_office".into(),"office@school.example.edu".into(),target.url.clone()));
        let coaches: Vec<(String,String)> = sqlx::query_as("SELECT s.source_id,c.email FROM contacts c JOIN schools s ON s.id=c.school_id WHERE c.kind='coach' ORDER BY c.email").fetch_all(&test.pool).await?;
        assert_eq!(coaches, vec![("1234".into(),"ada.runner@school.example.edu".into()),("1234".into(),"ben.walker@school.example.edu".into()),("1234".into(),"chris.thrower@school.example.edu".into())]);
        database.close().await;
        Ok(())
    })).await
}

fn contact(source: Source, url: &str, id: &str) -> Target {
    Target::get(
        source,
        PageKind::Contacts,
        url.into(),
        PageContext {
            school_id: Some(id.into()),
            school_name: Some("Example School".into()),
            season: Some(2026),
            ..PageContext::default()
        },
    )
}

#[tokio::test]
async fn given_blocked_contact_authority_when_pending_and_new_jobs_are_selected_then_only_that_site_stops()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let blocked = contact(Source::MileSplit,"https://School.Example.edu:443/staff","1234");
        let pending = contact(Source::AthleticNet,"https://school.example.edu/contact","9876");
        database.enqueue(&[blocked, pending], false).await?;
        let job = database.next_job(Some(Source::MileSplit), 3, &[]).await?.context("first contact")?;
        let input = test.directory().join("contact-challenge.html");
        std::fs::write(&input, "<html><head><title>Just a moment...</title></head><body><h1>Verify you are human</h1></body></html>")?;
        let error = Runner::new().replay(&Config::default(), &test.url, &input, job.target.clone())
            .await.expect_err("full-page challenge must stop this contact site");
        assert!(format!("{error:#}").contains("Official school website access challenge"));
        assert!(Runner::new().replay(&Config::default(), &test.url, &input, job.target.clone()).await.is_err());
        let fresh = contact(Source::MileSplit,"https://SCHOOL.EXAMPLE.EDU/athletics","later");
        let unrelated = contact(Source::MileSplit,"https://another.example.edu/staff","other");
        database.enqueue(&[fresh, unrelated.clone(), athletics_target()?], false).await?;
        let pending_id: i64 = sqlx::query_scalar("SELECT id FROM scrape_pages WHERE source='athleticnet'").fetch_one(&test.pool).await?;
        assert!(!database.start_attempt(pending_id, 3).await?);
        assert!(database.next_job(Some(Source::AthleticNet), 3, &[]).await?.is_none());
        let other = database.next_job_with_priority(Some(Source::MileSplit), 3, JobClass::Contacts, &[]).await?.context("other school progresses")?;
        assert_eq!(other.target.url, unrelated.url);
        assert!(database.start_attempt(other.id, 3).await?);
        save(&database, Some(other.id), &other.target, "", &ParsedPage::default()).await?;
        assert_eq!(database.next_job(Some(Source::MileSplit), 3, &[]).await?.context("provider continues")?.target.kind, PageKind::Team);
        let state: (String,i32,String) = sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE id=$1").bind(job.id).fetch_one(&test.pool).await?;
        assert_eq!(state, ("blocked".into(),1,"HTTP 200: parsing source page: Official school website access challenge; no alternate transport attempted".into()));
        let untouched: Vec<i32> = sqlx::query_scalar("SELECT attempts FROM scrape_pages WHERE cursor->'context'->>'school_id' IN ('9876','later')").fetch_all(&test.pool).await?;
        assert_eq!(untouched, vec![0,0]);
        let diagnostics: Vec<Vec<String>> = sqlx::query_scalar("SELECT notes FROM scrape_pages WHERE cursor->'context'->>'school_id' IN ('9876','later')")
            .fetch_all(&test.pool).await?;
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().all(|notes| notes.len() == 1 && notes.iter().all(|note|
            note.contains("Contact acquisition stopped by blocked site checkpoint")
                && note.contains("Official school website access challenge"))));
        database.close().await;
        Ok(())
    })).await
}

async fn prepare_startup(database: &Database, config: &Config) -> Result<()> {
    let seeds = sources::seeds(Source::MileSplit, &config.states, config.season)?;
    database.enqueue(&seeds, false).await?;
    sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=1,last_error='provider stop for network-free startup scenario'")
        .execute(database.pool()).await?;
    insert_school(
        database,
        school("1234", "Example High School", "https://school.example.edu/"),
    )
    .await
}

#[tokio::test]
async fn given_saved_website_when_normal_scrape_restarts_then_startup_backfills_under_lock_without_refreshing_contacts()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let mut config = Config::default();
        config.states = vec!["CA".into()];
        config.sources.athleticnet.enabled = false;
        prepare_startup(&database, &config).await?;
        let mut lock = database.pool().acquire().await?;
        sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),194832741)").execute(&mut *lock).await?;
        assert!(Runner::new().scrape(&config, &test.url).await.expect_err("normal scrape lock")
            .to_string().contains("another scraper owns"));
        assert!(checkpoint_rows(&database).await?.is_empty());
        sqlx::query("SELECT pg_advisory_unlock(hashtext(current_schema()),194832741)").execute(&mut *lock).await?;
        drop(lock);
        let error = Runner::new().scrape(&config, &test.url).await.expect_err("provider stop avoids network");
        assert!(error.to_string().contains("blocked page requiring human review"));
        let rows = checkpoint_rows(&database).await?;
        assert_eq!(rows, vec![("1234".into(),"pending".into(),0)]);
        sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=1,last_error='contact challenge' WHERE cursor->>'kind'='contacts'").execute(&test.pool).await?;
        sqlx::query("UPDATE scrape_pages SET status='succeeded' WHERE cursor->>'kind' <> 'contacts'").execute(&test.pool).await?;
        Runner::new().scrape_with_refresh(&config, &test.url, false).await?;
        assert_eq!(checkpoint_rows(&database).await?, vec![("1234".into(),"blocked".into(),1)]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_public_contacts_with_login_links_and_form_captcha_when_parsed_then_published_records_are_not_blocked()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let school = school("1234", "Example High School", "https://school.example.edu/");
        insert_school(&database, school.clone()).await?;
        let target = sources::contacts::targets(Source::MileSplit, &school, 2026)?
            .into_iter().next().context("official target")?;
        let body = format!("{}<aside><a href='/signin'>Sign in to continue</a><form><div class='g-recaptcha'>Verify you are human</div></form></aside>",
            include_str!("../fixtures/contacts/scoped-school.html"));
        let input = test.directory().join("public-contacts-with-captcha.html");
        std::fs::write(&input, body)?;
        Runner::new().replay(&Config::default(), &test.url, &input, target.clone()).await?;
        let state: String = sqlx::query_scalar("SELECT status FROM scrape_pages WHERE url=$1")
            .bind(&target.url).fetch_one(&test.pool).await?;
        assert_eq!(state, "succeeded");
        let office: String = sqlx::query_scalar("SELECT email FROM contacts WHERE kind='school_office'").fetch_one(&test.pool).await?;
        assert_eq!(office, "office@school.example.edu");
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_published_scheme_less_school_websites_when_discovered_then_https_requests_preserve_observed_source_values()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = athletics_target()?;
        for (published, request) in [
            ("school.example.edu/athletics/staff?view=all", "https://school.example.edu/athletics/staff?view=all"),
            ("www.school.example.edu", "https://www.school.example.edu/"),
        ] {
            let body = athletics_body(published);
            let page = sources::parse(&target, &body)?;
            save(&database, None, &target, &body, &page).await?;
            let observation: (String, String, String) = sqlx::query_as(
                "SELECT source,source_id,website FROM schools WHERE source='milesplit' AND source_id='1234'",
            ).fetch_one(&test.pool).await?;
            assert_eq!(observation, ("milesplit".into(), "1234".into(), published.into()));
            let checkpoint: (String, String, String, i32, i32) = sqlx::query_as(
                "SELECT source,cursor->'context'->>'school_id',status,attempts, \
                 (cursor->'context'->>'season')::int FROM scrape_pages \
                 WHERE cursor->>'kind'='contacts' AND url=$1",
            ).bind(request).fetch_one(&test.pool).await?;
            assert_eq!(checkpoint, ("milesplit".into(), "1234".into(), "pending".into(), 0, 2025));
        }
        assert_eq!(database.backfill_contacts(&["CA".into()], &[Source::MileSplit], 2026).await?, 0);
        insert_school(&database, school("5678", "Saved School", "backfill.example.edu/staff")).await?;
        assert_eq!(database.backfill_contacts(&["CA".into()], &[Source::MileSplit], 2026).await?, 1);
        let saved: (String, String, String) = sqlx::query_as(
            "SELECT s.website,p.url,s.source_id FROM schools s JOIN scrape_pages p \
             ON p.source=s.source AND p.cursor->'context'->>'school_id'=s.source_id \
             WHERE s.source='milesplit' AND s.source_id='5678' AND p.cursor->>'kind'='contacts'",
        ).fetch_one(&test.pool).await?;
        assert_eq!(saved, ("backfill.example.edu/staff".into(), "https://backfill.example.edu/staff".into(), "5678".into()));
        let contacts: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE cursor->>'kind'='contacts'")
            .fetch_one(&test.pool).await?;
        assert_eq!(contacts, 3);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_public_staff_and_office_with_credential_markup_when_replayed_then_only_published_records_are_durable()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let canary = "transient-contact-secret-9f37";
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0cmFja2VyLXRlc3QifQ.signature";
        for (id, url, fixture, expected) in [
            ("1234", "https://gomats.org/staff-directory", include_str!("../fixtures/contacts/miramonte-staff.html"),
             vec!["bhenderson@auhsdschools.org", "caljumper259@gmail.com", "dharris@auhsdschools.org"]),
            ("5678", "https://school.example.edu/athletics/staff", include_str!("../fixtures/contacts/scoped-school.html"),
             vec!["ada.runner@school.example.edu", "ben.walker@school.example.edu", "chris.thrower@school.example.edu", "office@school.example.edu"]),
        ] {
            let name = if id == "1234" { "Miramonte High School" } else { "Example High School" };
            let school = school(id, name, url);
            insert_school(&database, school.clone()).await?;
            let target = sources::contacts::targets(Source::MileSplit, &school, 2026)?
                .into_iter().next().context("official contact target")?;
            let fixture = fixture.replace(
                "\"url\":\"https://school.example.edu/\"",
                &format!("\"url\":\"https://school.example.edu/?access_token={canary}\""),
            );
            let markup = format!("<a href='{url}?public_route={jwt}'>Track staff</a>{fixture}<aside><form><input type='password' value='{canary}'></form></aside><script>const userToken = getToken('{canary}');</script>");
            assert!(track_scrapper_v2::transport::redact_capture(Capture {
                url: url.into(), status: 200, content_type: "text/html".into(),
                body: markup.as_bytes().to_vec(),
            }).is_err());
            let input = test.directory().join(format!("transient-{id}.html"));
            std::fs::write(&input, markup)?;
            Runner::new().replay(&Config::default(), &test.url, &input, target).await?;
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT c.email FROM contacts c JOIN schools s ON s.id=c.school_id WHERE s.source_id=$1 ORDER BY c.email",
            ).bind(id).fetch_all(&test.pool).await?;
            assert_eq!(rows, expected);
            let checkpoint: (String, i32) = sqlx::query_as(
                "SELECT status,attempts FROM scrape_pages WHERE url=$1",
            ).bind(url).fetch_one(&test.pool).await?;
            assert_eq!(checkpoint, ("succeeded".into(), 1));
        }
        let office: (String, String) = sqlx::query_as(
            "SELECT kind,email FROM contacts WHERE kind='school_office'",
        ).fetch_one(&test.pool).await?;
        let postal: (String, String, String) = sqlx::query_as(
            "SELECT address,postal_code,website FROM schools WHERE source_id='5678'",
        ).fetch_one(&test.pool).await?;
        assert_eq!(postal, ("123 School Road".into(), "90210".into(), "https://school.example.edu/athletics/staff".into()));
        assert_eq!(office, ("school_office".into(), "office@school.example.edu".into()));
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT row_to_json(t)::text FROM schools t \
             UNION ALL SELECT row_to_json(t)::text FROM athletes t \
             UNION ALL SELECT row_to_json(t)::text FROM athlete_sources t \
             UNION ALL SELECT row_to_json(t)::text FROM results t \
             UNION ALL SELECT row_to_json(t)::text FROM contacts t \
             UNION ALL SELECT row_to_json(t)::text FROM scrape_pages t"
        ).fetch_all(&test.pool).await?;
        assert!(rows.iter().all(|row| !row.contains(canary) && !row.contains(jwt) && !row.contains("getToken(") && !row.contains("<html")));
        database.close().await;
        Ok(())
    })).await
}
