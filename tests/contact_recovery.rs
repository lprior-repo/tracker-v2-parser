mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::{Database, Job, JobClass},
    model::{Capture, PageContext, PageKind, Source, Target},
};

fn target(path: &str, school: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        format!("https://{school}.example.edu/{path}"),
        PageContext {
            school_id: Some(school.into()),
            school_name: Some(format!("{school} High School")),
            state: Some("WI".into()),
            season: Some(2026),
            subject_id: Some("contacts-depth:0".into()),
            ..PageContext::default()
        },
    )
}

fn response(target: &Target, status: u16) -> Capture {
    Capture {
        url: target.url.clone(),
        status,
        content_type: "text/html".into(),
        body: b"<main>Unavailable page</main>".to_vec(),
    }
}

async fn begin(database: &Database, target: &Target) -> Result<Job> {
    database
        .enqueue(std::slice::from_ref(target), false)
        .await?;
    let job = database
        .next_job(None, 3, &["WI".into()])
        .await?
        .context("new contact checkpoint")?;
    assert_eq!(job.target.url, target.url);
    assert!(database.start_attempt(job.id, 3).await?);
    Ok(job)
}

#[tokio::test]
async fn given_legacy_contact_path_when_404_is_recorded_then_same_site_root_resumes_without_attempt_reset()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let original = target("High/HSIndex.html?pid=50", "school");
            let job = begin(&database, &original).await?;
            database
                .fail_job(
                    &job,
                    Some(&response(&original, 404)),
                    "published page not found",
                    false,
                )
                .await?;
            let root = database
                .next_job_with_priority(None, 3, JobClass::Contacts, &["WI".into()])
                .await?
                .context("same-site root")?;
            assert_eq!(root.target.url, "https://school.example.edu/");
            assert_eq!(root.target.context.school_id.as_deref(), Some("school"));
            assert_eq!(
                root.target.context.school_name.as_deref(),
                Some("school High School")
            );
            assert_eq!(root.target.context.state.as_deref(), Some("WI"));
            assert_eq!(root.target.context.season, Some(2026));
            assert_eq!(
                root.target.context.subject_id.as_deref(),
                Some("contacts-depth:0")
            );
            let original_state: (String, i32, String) =
                sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE id=$1")
                    .bind(job.id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                original_state,
                (
                    "failed".into(),
                    1,
                    "HTTP 404: published page not found".into()
                )
            );
            for _ in 1..3 {
                assert!(database.start_attempt(job.id, 3).await?);
                database
                    .fail_job(
                        &job,
                        Some(&response(&original, 404)),
                        "published page not found",
                        false,
                    )
                    .await?;
            }
            assert!(!database.start_attempt(job.id, 3).await?);
            database
                .enqueue(std::slice::from_ref(&original), true)
                .await?;
            let rows: Vec<(String, String, i32)> =
                sqlx::query_as("SELECT url,status,attempts FROM scrape_pages ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(
                rows,
                vec![
                    (original.url.clone(), "failed".into(), 3),
                    (root.target.url.clone(), "pending".into(), 0)
                ]
            );
            let notes: Vec<String> =
                sqlx::query_scalar("SELECT notes FROM scrape_pages WHERE id=$1")
                    .bind(job.id)
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(notes.len(), 1);
            assert!(notes[0].contains("https://school.example.edu/"));
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_access_stop_other_error_or_noninitial_page_when_failure_is_saved_then_no_recovery_request_is_invented()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            for (status, blocked, id) in [
                (400, false, "bad-request"),
                (401, true, "auth"),
                (403, true, "denied"),
                (429, true, "limited"),
                (500, false, "server"),
            ] {
                let original = target("old/path", id);
                let job = begin(&database, &original).await?;
                database
                    .fail_job(
                        &job,
                        Some(&response(&original, status)),
                        "source error",
                        blocked,
                    )
                    .await?;
            }
            for (id, path, depth) in [
                ("root", "", "contacts-depth:0"),
                ("child", "staff/person", "contacts-depth:1"),
                ("deep", "staff/team", "contacts-depth:2"),
            ] {
                let mut original = target(path, id);
                original.context.subject_id = Some(depth.into());
                let job = begin(&database, &original).await?;
                database
                    .fail_job(&job, Some(&response(&original, 404)), "not found", false)
                    .await?;
            }
            let mut provider = target("teams/1", "provider");
            provider.kind = PageKind::Team;
            provider.url = "https://wi.milesplit.com/teams/1".into();
            let job = begin(&database, &provider).await?;
            database
                .fail_job(&job, Some(&response(&provider, 404)), "not found", false)
                .await?;
            let rows: Vec<(String, i32)> =
                sqlx::query_as("SELECT status,attempts FROM scrape_pages ORDER BY id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(
                rows,
                vec![
                    ("failed".into(), 1),
                    ("blocked".into(), 1),
                    ("blocked".into(), 1),
                    ("blocked".into(), 1),
                    ("failed".into(), 1),
                    ("failed".into(), 1),
                    ("failed".into(), 1),
                    ("failed".into(), 1),
                    ("failed".into(), 1)
                ]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_root_checkpoint_insert_failure_when_404_is_saved_then_failure_and_recovery_roll_back_together()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let original = target("legacy/page","school");
        let job = begin(&database,&original).await?;
        sqlx::raw_sql("CREATE FUNCTION reject_recovery_root() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.url='https://school.example.edu/' THEN RAISE EXCEPTION 'root checkpoint rejected'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_recovery_root BEFORE INSERT ON scrape_pages FOR EACH ROW EXECUTE FUNCTION reject_recovery_root();")
            .execute(&test.pool).await?;
        assert!(database.fail_job(&job,Some(&response(&original,404)),"not found",false).await.is_err());
        let rows: Vec<(String,String,i32,Option<String>,Vec<String>)> = sqlx::query_as("SELECT url,status,attempts,last_error,notes FROM scrape_pages ORDER BY id")
            .fetch_all(&test.pool).await?;
        assert_eq!(rows,vec![(original.url,"pending".into(),1,None,vec![])]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_existing_completed_root_when_multiple_legacy_paths_fail_then_root_and_attempts_are_preserved()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        for slash in [true,false] {
            let school = if slash { "slash" } else { "pathless" };
            let mut root = target("",school);
            if !slash { root.url = format!("https://{school}.example.edu"); }
            let root_job = begin(&database,&root).await?;
            database.persist(Some(root_job.id),&root,&response(&root,200),&Default::default()).await?;
            for path in ["old/first.htm","old/second.htm"] {
                let legacy = target(path,school);
                let job = begin(&database,&legacy).await?;
                database.fail_job(&job,Some(&response(&legacy,404)),"not found",false).await?;
            }
            let rows: Vec<(String,String,i32)> = sqlx::query_as(
                "SELECT url,status,attempts FROM scrape_pages WHERE cursor->'context'->>'school_id'=$1 ORDER BY id",
            ).bind(school).fetch_all(&test.pool).await?;
            assert_eq!(rows,vec![
                (root.url,"succeeded".into(),1),
                (format!("https://{school}.example.edu/old/first.htm"),"failed".into(),1),
                (format!("https://{school}.example.edu/old/second.htm"),"failed".into(),1),
            ]);
        }
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_rendered_contact_challenge_when_cli_replays_then_site_is_blocked_without_saving_empty_success()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let page = target("staff","school");
        let input = test.directory().join("rendered-challenge.html");
        std::fs::write(&input,"<html><head><title>School</title></head><body><main>captcha challenge</main></body></html>")?;
        let output = test.cli(&[
            "scrape","--source","milesplit","--kind","contacts","--url",&page.url,
            "--input",input.to_str().context("UTF8 capture path")?,"--school-id","school",
            "--school-name","school High School","--state","WI","--season","2026",
        ]).await?;
        assert!(!output.status.success());
        let state: (String,i32,Option<String>) = sqlx::query_as(
            "SELECT status,attempts,last_success::text FROM scrape_pages WHERE url=$1",
        ).bind(&page.url).fetch_one(&test.pool).await?;
        assert_eq!(state,("blocked".into(),1,None));
        let pending = target("athletics","school");
        database.enqueue(&[pending],false).await?;
        assert!(database.next_job(None,3,&["WI".into()]).await?.is_none());
        let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts").fetch_one(&test.pool).await?;
        assert_eq!(claims,0);
        database.close().await;
        Ok(())
    })).await
}
