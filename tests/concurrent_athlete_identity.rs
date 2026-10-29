#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    durable::page::{self, ArtifactRef, CaptureRef, PageOutcome, PageReservation, ParsedRef},
    model::{Athlete, Capture, ParsedPage, Sport},
};

struct Prepared {
    reservation: PageReservation,
    capture: Capture,
    captured: CaptureRef,
    parsed: ParsedPage,
    parsed_ref: ParsedRef,
}

impl Prepared {
    async fn persist(&self, database: &Database) -> Result<PageOutcome> {
        database
            .durable_persist(
                &self.reservation,
                &self.captured,
                &self.parsed_ref,
                &self.capture,
                &self.parsed,
            )
            .await
    }
}

#[tokio::test]
async fn simultaneous_first_identity_discovery_keeps_both_pages_and_receipt_replays() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let scope = page_support::scope(&page_support::config()?)?;
            let first = prepare(&database, &scope, "101", "first-result", "11.11").await?;
            let second = prepare(&database, &scope, "102", "second-result", "11.12").await?;
            assert_ne!(
                first.reservation.request.binding.page_id,
                second.reservation.request.binding.page_id
            );
            assert_ne!(first.reservation.attempt_id, second.reservation.attempt_id);
            sqlx::raw_sql(
                "CREATE FUNCTION hold_new_identity() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318321); \
             RETURN NEW; END $$; \
             CREATE TRIGGER hold_new_identity BEFORE INSERT ON athlete_sources \
             FOR EACH ROW EXECUTE FUNCTION hold_new_identity();",
            )
            .execute(&test.pool)
            .await?;
            let mut gate = test.pool.acquire().await?;
            sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),7318321)")
                .execute(&mut *gate)
                .await?;
            let (left, right, released) = tokio::join!(
                first.persist(&database),
                second.persist(&database),
                release_after_both_insertions(&test.pool, &mut gate),
            );
            released?;
            let outcomes = [left?, right?];
            assert!(
                outcomes
                    .iter()
                    .all(|outcome| matches!(outcome, PageOutcome::Completed { .. }))
            );
            let counts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM athletes), (SELECT count(*) FROM athlete_sources), \
             (SELECT count(*) FROM results), (SELECT count(*) FROM durable_page_receipts), \
             (SELECT count(*) FROM scrape_pages WHERE status='succeeded'), \
             (SELECT count(*) FROM athletes a WHERE NOT EXISTS \
             (SELECT 1 FROM athlete_sources p WHERE p.athlete_id=a.id))",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(counts, (1, 1, 2, 2, 2, 0));
            let marks: Vec<String> =
                sqlx::query_scalar("SELECT raw_mark FROM results ORDER BY raw_mark")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(marks, ["11.11", "11.12"]);
            let owned: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id \
             WHERE r.athlete_id=p.athlete_id AND p.source='milesplit' AND p.source_id='424242'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(owned, 2);
            let before = page_support::snapshots(&database).await?;
            database.close().await;
            let restarted = Database::connect(&test.url).await?;
            assert_eq!(first.persist(&restarted).await?, outcomes[0]);
            assert_eq!(second.persist(&restarted).await?, outcomes[1]);
            assert_eq!(page_support::snapshots(&restarted).await?, before);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_wire_requests")
                    .fetch_one(&test.pool)
                    .await?,
                0
            );
            restarted.close().await;
            Ok(())
        })
    })
    .await
}

async fn release_after_both_insertions(
    pool: &sqlx::PgPool,
    gate: &mut sqlx::PgConnection,
) -> Result<()> {
    let waiting = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks WHERE locktype='advisory' \
                 AND classid::bigint=(hashtext(current_schema())::bigint & 4294967295) \
                 AND objid=7318321 AND NOT granted",
            )
            .fetch_one(pool)
            .await?;
            if count == 2 {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let unlocked: bool =
        sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext(current_schema()),7318321)")
            .fetch_one(gate)
            .await?;
    assert!(unlocked);
    waiting
        .context("both independent page transactions must reach the absent identity insert")??;
    Ok(())
}

async fn prepare(
    database: &Database,
    scope: &page::PageScope,
    school: &str,
    result: &str,
    mark: &str,
) -> Result<Prepared> {
    let target = page_support::target(school);
    let request = page_support::request(database, &target, scope).await?;
    let reservation =
        page_support::reserve(database, &request, chrono::Utc::now().timestamp_millis()).await?;
    let capture = Capture {
        url: target.url,
        status: 200,
        content_type: "text/html".into(),
        body: format!("<html><body>{result}</body></html>").into_bytes(),
    };
    let captured = CaptureRef {
        body: artifact(&capture.body)?,
        url: capture.url.clone(),
        status: capture.status,
        content_type: capture.content_type.clone(),
        retained_at_ms: 1_793_318_400_000,
    };
    let parsed: ParsedPage = serde_json::from_value(serde_json::json!({
        "athletes": [Athlete { id: "424242".into(), name: "Concurrent Athlete".into(),
            state: Some("OH".into()), graduation_year: Some(2027), ..Default::default() }],
        "performances": [{"athlete_id":"424242","event":"100m","mark":mark,
            "sport":Sport::Outdoor,"result_id":result,"school_id":null,"meet_id":null,
            "meet_name":null,"date":null,"wind":null,"timing":null,"course":null,
            "is_personal_record":false,"is_relay":false,"season":2026,"grade":null,"gender":null}],
        "schools":[],"coaches":[],"next":[],"notes":[]
    }))?;
    let parsed_ref = ParsedRef {
        body: artifact(&serde_json::to_vec(&parsed)?)?,
        capture_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(&captured)?)),
    };
    database
        .durable_record_capture(&reservation, &captured)
        .await?;
    database
        .durable_record_parse(&reservation, &parsed_ref)
        .await?;
    Ok(Prepared {
        reservation,
        capture,
        captured,
        parsed,
        parsed_ref,
    })
}

fn artifact(bytes: &[u8]) -> Result<ArtifactRef> {
    Ok(ArtifactRef {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        bytes: u64::try_from(bytes.len())?,
    })
}

#[tokio::test]
async fn pooled_connections_bound_row_lock_waits_and_retry_after_release() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let configured: String = sqlx::query_scalar("SHOW lock_timeout")
                .fetch_one(database.pool())
                .await?;
            assert_eq!(configured, "5s");
            let schema: String = sqlx::query_scalar("SELECT current_schema()")
                .fetch_one(database.pool())
                .await?;
            assert!(schema.starts_with("scenario_"));
            let athlete: i64 =
                sqlx::query_scalar("INSERT INTO athletes DEFAULT VALUES RETURNING id")
                    .fetch_one(database.pool())
                    .await?;
            let mut held = test.pool.begin().await?;
            sqlx::query("SELECT id FROM athletes WHERE id=$1 FOR UPDATE")
                .bind(athlete)
                .execute(&mut *held)
                .await?;
            let mut blocked = database.pool().begin().await?;
            sqlx::query("SET LOCAL lock_timeout='50ms'")
                .execute(&mut *blocked)
                .await?;
            let error = sqlx::query("UPDATE athletes SET name='Released Athlete' WHERE id=$1")
                .bind(athlete)
                .execute(&mut *blocked)
                .await
                .expect_err("held row must time out");
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("55P03")
            );
            blocked.rollback().await?;
            held.rollback().await?;
            assert_eq!(
                sqlx::query("UPDATE athletes SET name='Released Athlete' WHERE id=$1")
                    .bind(athlete)
                    .execute(database.pool())
                    .await?
                    .rows_affected(),
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, String>("SELECT name FROM athletes WHERE id=$1")
                    .bind(athlete)
                    .fetch_one(database.pool())
                    .await?,
                "Released Athlete"
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
