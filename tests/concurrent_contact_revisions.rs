#[path = "support/records.rs"]
mod records;
mod support;

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Target},
    sources,
};

const STAFF: &str = include_str!("../fixtures/contacts/miramonte-staff.html");
const FINAL_URL: &str = "https://gomats.org/staff-directory";
const EARLIER: &str = "Earlier contact source claim; not confirmed by latest parse";

#[tokio::test]
async fn given_concurrent_redirected_staff_captures_when_revised_then_only_latest_claims_are_current()
-> Result<()> {
    scenario(false).await
}

#[tokio::test]
async fn given_uncommitted_staff_capture_when_later_capture_is_empty_then_prior_claims_need_review()
-> Result<()> {
    scenario(true).await
}

#[tokio::test]
async fn given_absent_school_when_first_office_capture_overlaps_empty_capture_then_mailbox_is_withdrawn()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM schools")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(existing, 0);
            let first = Revision::new(
                "https://gomats.org/athletics",
                "<title>Miramonte High School Athletics</title><main><h1>Miramonte High School</h1><p>Main Office: <a href='mailto:office@gomats.org'>Email office</a></p></main>",
            )?;
            let second = Revision::new(
                "https://gomats.org/coaches",
                "<title>Miramonte High School</title><main>No current office mailbox is published.</main>",
            )?;
            assert_eq!(first.parsed.schools.len(), 1);
            let school = first.parsed.schools.first().context("parsed office school")?;
            assert_eq!(school.id, "school-123");
            assert_eq!(school.public_email.as_deref(), Some("office@gomats.org"));
            assert!(first.parsed.coaches.is_empty());
            assert!(second.parsed.schools.is_empty() && second.parsed.coaches.is_empty());
            interleave(&database, &test.pool, &first, &second).await?;
            let public_email: Option<String> = sqlx::query_scalar(
                "SELECT public_email FROM schools WHERE source='athleticnet' AND source_id='school-123'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(public_email, None);
            let before = contacts(&test.pool).await?;
            assert_eq!(before.len(), 1);
            let office = before.first().context("historical office claim")?;
            assert_eq!(office["kind"], "school_office");
            assert_eq!(office["email"], "office@gomats.org");
            assert_eq!(office["source_url"], FINAL_URL);
            assert_eq!(office["raw"]["review_notes"], serde_json::json!([EARLIER]));
            second.persist(&database).await?;
            assert_eq!(contacts(&test.pool).await?, before);
            let replayed_email: Option<String> = sqlx::query_scalar(
                "SELECT public_email FROM schools WHERE source='athleticnet' AND source_id='school-123'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(replayed_email, None);
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn scenario(empty: bool) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database).await?;
            let first = Revision::new("https://gomats.org/athletics", STAFF)?;
            let body = if empty {
                "<html><title>Staff directory</title><main>No current coaching assignments.</main></html>".into()
            } else {
                STAFF.replace("\"bhenderson\"", "\"current-brian\"")
                    .replace("\"caljumper259\"", "\"current-robert\"")
                    .replace("\"dharris\"", "\"current-dax\"")
            };
            let second = Revision::new("https://gomats.org/coaches", &body)?;
            assert_eq!(first.parsed.coaches.len(), 3);
            assert_eq!(second.parsed.coaches.len(), if empty { 0 } else { 3 });
            assert!(first.parsed.schools.is_empty() && second.parsed.schools.is_empty());
            assert!(second.parsed.coaches.iter().all(|current| first.parsed.coaches.iter()
                .all(|prior| current.email != prior.email)));
            interleave(&database, &test.pool, &first, &second).await?;
            assert_claims(&database, &second, empty).await?;
            let before = contacts(&test.pool).await?;
            second.persist(&database).await?;
            let after = contacts(&test.pool).await?;
            assert_eq!(after, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}

async fn seed(database: &Database) -> Result<()> {
    let mut athlete = records::athlete("202811");
    athlete.state = Some("CA".into());
    athlete.school_id = Some("school-123".into());
    athlete.graduation_year = Some(2028);
    records::persist(
        database,
        &records::target(Source::AthleticNet, "202811"),
        &ParsedPage {
            schools: vec![School {
                id: "school-123".into(),
                name: "Miramonte High School".into(),
                state: Some("CA".into()),
                ..Default::default()
            }],
            athletes: vec![athlete],
            ..Default::default()
        },
    )
    .await
}

struct Revision {
    target: Target,
    capture: Capture,
    parsed: ParsedPage,
}

impl Revision {
    fn new(request_url: &str, body: &str) -> Result<Self> {
        let target = Target::get(
            Source::AthleticNet,
            PageKind::Contacts,
            request_url.into(),
            PageContext {
                school_id: Some("school-123".into()),
                school_name: Some("Miramonte High School".into()),
                state: Some("CA".into()),
                season: Some(2026),
                subject_id: Some("contacts-depth:0".into()),
                ..Default::default()
            },
        );
        let mut effective = target.clone();
        effective.url = FINAL_URL.into();
        let parsed = sources::parse(&effective, body)?;
        Ok(Self {
            target,
            capture: Capture {
                url: FINAL_URL.into(),
                status: 200,
                content_type: "text/html".into(),
                body: body.as_bytes().to_vec(),
            },
            parsed,
        })
    }

    async fn persist(&self, database: &Database) -> Result<i64> {
        database
            .persist(None, &self.target, &self.capture, &self.parsed)
            .await
    }
}

async fn interleave(
    database: &Database,
    pool: &PgPool,
    first: &Revision,
    second: &Revision,
) -> Result<()> {
    let mut held = database.pool().begin().await?;
    let owner: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await?;
    Database::persist_transaction(
        &mut held,
        None,
        &first.target,
        &first.capture,
        &first.parsed,
    )
    .await?;
    let complete = AtomicBool::new(false);
    let later = async {
        let result = second.persist(database).await;
        complete.store(true, Ordering::Release);
        result
    };
    let release = async {
        let ready = completion_or_blocked(pool, owner, &complete).await;
        held.commit().await?;
        ready
    };
    let (written, released) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(later, release)
    })
    .await
    .context("contact revisions exceeded 10 seconds")?;
    released?;
    written?;
    Ok(())
}

async fn completion_or_blocked(pool: &PgPool, owner: i32, complete: &AtomicBool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if complete.load(Ordering::Acquire) {
                return Ok(());
            }
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(owner)
            .fetch_one(pool)
            .await?;
            if blocked {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("second contact revision neither completed nor blocked")?
}

async fn assert_claims(database: &Database, latest: &Revision, empty: bool) -> Result<()> {
    let mut expected = latest
        .parsed
        .coaches
        .iter()
        .filter_map(|coach| coach.email.clone())
        .collect::<Vec<_>>();
    expected.sort();
    let current: Vec<String> = sqlx::query_scalar(
        "SELECT email FROM contacts WHERE kind='coach' AND NOT COALESCE(raw->'review_notes','[]'::jsonb) @> jsonb_build_array($1::text) ORDER BY email",
    ).bind(EARLIER).fetch_all(database.pool()).await?;
    assert_eq!(
        current, expected,
        "overlapping captures retained stale current coaching claims"
    );
    let earlier: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM contacts WHERE kind='coach' AND COALESCE(raw->'review_notes','[]'::jsonb) @> jsonb_build_array($1::text)",
    ).bind(EARLIER).fetch_one(database.pool()).await?;
    assert_eq!(earlier, 3);
    let exported = database
        .athlete_records_for(&["CA".into()], &[2028])
        .await?;
    assert_eq!(exported.len(), 1);
    let coaches = &exported.first().context("exported 2028 athlete")?.coaches;
    assert_eq!(coaches.len(), if empty { 3 } else { 6 });
    assert_eq!(
        coaches
            .iter()
            .filter(|coach| coach.review_notes.iter().any(|note| note == EARLIER))
            .count(),
        3
    );
    Ok(())
}

async fn contacts(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(c)-'last_checked' FROM contacts c ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}
