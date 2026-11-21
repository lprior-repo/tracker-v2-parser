#[path = "durable_page/support.rs"]
mod page_support;
#[path = "support/records.rs"]
mod records;
mod support;

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::page::{ArtifactRef, CaptureRef, PageOutcome, PageReservation, ParsedRef},
    model::{Capture, ParsedPage, Source},
};

const GATE: i32 = 7318323;

#[tokio::test]
async fn given_a_linked_pair_when_both_profiles_split_then_no_phantom_athlete_is_exported()
-> Result<()> {
    scenario(false).await
}

#[tokio::test]
async fn given_conflicting_same_as_updates_when_retried_then_split_ownership_and_receipts_remain_stable()
-> Result<()> {
    scenario(true).await
}

#[tokio::test]
async fn given_reviewed_profiles_when_unaccepted_names_split_concurrently_then_historical_audit_is_retained()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed_reviewed(test, &database).await?;
            let before = identity_audit(&test.pool).await?;
            let first = Prepared::named(&database, "11", "Unaccepted First Name").await?;
            let second = Prepared::named(&database, "21", "Unaccepted Second Name").await?;
            let outcomes = interleave(&database, &test.pool, &first, &second).await?;
            assert_eq!(identity_audit(&test.pool).await?, before);
            let counts: (i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM athletes), \
                 (SELECT count(*) FROM athlete_sources WHERE details ? '_reviewed_identity'), \
                 (SELECT count(*) FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id \
                    WHERE r.athlete_id=p.athlete_id)",
            ).fetch_one(&test.pool).await?;
            assert_eq!(counts, (2, 2, 2));
            assert_eq!(first.persist(&database).await?, outcomes[0]);
            assert_eq!(second.persist(&database).await?, outcomes[1]);
            assert_eq!(identity_audit(&test.pool).await?, before);
            database.close().await;
            Ok(())
        })
    }).await
}

async fn scenario(retain_links: bool) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database).await?;
            let first = Prepared::new(&database, Source::AthleticNet, "11", 2027, retain_links).await?;
            let second = Prepared::new(&database, Source::MileSplit, "21", 2029, retain_links).await?;
            let outcomes = interleave(&database, &test.pool, &first, &second).await?;
            let before = page_support::snapshots(&database).await?;
            database.close().await;
            let restarted = Database::connect(&test.url).await?;
            assert_eq!(first.persist(&restarted).await?, outcomes[0]);
            assert_eq!(second.persist(&restarted).await?, outcomes[1]);
            assert_eq!(page_support::snapshots(&restarted).await?, before);
            let facts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM athletes WHERE name IS NOT NULL), \
                 (SELECT count(*) FROM athlete_sources), (SELECT count(*) FROM results), \
                 (SELECT count(*) FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id \
                    WHERE r.athlete_id<>p.athlete_id), \
                 (SELECT count(*) FROM athletes a WHERE a.name IS NOT NULL \
                    AND NOT EXISTS(SELECT 1 FROM athlete_sources p WHERE p.athlete_id=a.id)), \
                 (SELECT count(*) FROM durable_page_receipts)",
            ).fetch_one(&test.pool).await?;
            let (exported, _) = restarted.export_snapshot(&["MI".into()], &[2028]).await?;
            let empty_exports = exported.iter().filter(|athlete| athlete.profiles.is_empty()).count();
            assert_eq!((facts, exported.len(), empty_exports), ((2, 2, 2, 0, 0, 2), 2, 0));
            assert!(exported.iter().all(|athlete| athlete.profiles.len()==1 && athlete.performances.len()==1));
            restarted.close().await;
            Ok(())
        })
    }).await
}

fn observation(source: Source, id: &str, year: i32, linked: bool) -> ParsedPage {
    let mut athlete = records::athlete(id);
    athlete.state = Some("MI".into());
    athlete.graduation_year = Some(year);
    athlete.profile_url = Some(records::target(source, id).url);
    if linked {
        athlete.same_as = vec![
            match source {
                Source::AthleticNet => "https://www.milesplit.com/athletes/21",
                Source::MileSplit => "https://www.athletic.net/athlete/11",
            }
            .into(),
        ];
    }
    let mut performance = records::performance("11.00");
    performance.athlete_id = id.into();
    performance.result_id = Some(format!("race-{id}"));
    ParsedPage {
        athletes: vec![athlete],
        performances: vec![performance],
        ..Default::default()
    }
}

async fn seed_reviewed(test: &support::Scenario, database: &Database) -> Result<()> {
    for id in ["11", "21"] {
        records::persist(
            database,
            &records::target(Source::MileSplit, id),
            &observation(Source::MileSplit, id, 2028, false),
        )
        .await?;
    }
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT source_id,athlete_id FROM athlete_sources ORDER BY id")
            .fetch_all(&test.pool)
            .await?;
    let manifest = json!({"schema_revision":1,"canonical_athlete_id":rows.iter().map(|row|row.1).min(),
        "preferred_name":"Alex Morgan","accepted_names":["Alex Morgan"],"reviewed_by":"user",
        "reason":"Fixture review confirming these two profiles identify one athlete",
        "profiles":rows.iter().map(|row|json!({"source":"milesplit","source_id":row.0,
            "expected_athlete_id":row.1})).collect::<Vec<_>>()});
    let path = test.directory().join("reviewed-split.json");
    let bytes = serde_json::to_vec(&manifest)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    std::fs::write(&path, bytes)?;
    let output = test
        .operation(&support::OfflineOperation::ReviewedIdentity {
            manifest: path,
            sha256: digest,
        })
        .await?;
    support::assert_success(&output);
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["profile_count"], 2);
    assert_eq!(report["result_count"], 2);
    Ok(())
}

async fn identity_audit(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT details->'_reviewed_identity' FROM athlete_sources ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

async fn seed(database: &Database) -> Result<()> {
    records::persist(
        database,
        &records::target(Source::AthleticNet, "11"),
        &observation(Source::AthleticNet, "11", 2028, false),
    )
    .await?;
    records::persist(
        database,
        &records::target(Source::MileSplit, "21"),
        &observation(Source::MileSplit, "21", 2028, true),
    )
    .await?;
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM athletes),(SELECT count(*) FROM athlete_sources), \
         (SELECT count(*) FROM results)",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(counts, (1, 2, 2));
    Ok(())
}

struct Prepared {
    reservation: PageReservation,
    capture: Capture,
    captured: CaptureRef,
    parsed: ParsedPage,
    parsed_ref: ParsedRef,
}

impl Prepared {
    async fn new(
        database: &Database,
        source: Source,
        id: &str,
        year: i32,
        linked: bool,
    ) -> Result<Self> {
        Self::prepare(database, source, id, observation(source, id, year, linked)).await
    }

    async fn named(database: &Database, id: &str, name: &str) -> Result<Self> {
        let mut page = observation(Source::MileSplit, id, 2028, false);
        page.athletes
            .first_mut()
            .context("named profile observation")?
            .name = name.into();
        Self::prepare(database, Source::MileSplit, id, page).await
    }

    async fn prepare(
        database: &Database,
        source: Source,
        id: &str,
        parsed: ParsedPage,
    ) -> Result<Self> {
        let mut target = records::target(source, id);
        target.context.state = Some("MI".into());
        let config = Config::parse("states=['MI']\ngraduation_years=[2028]\nseason=2026\n")?;
        let scope = page_support::scope(&config)?;
        let request = page_support::request(database, &target, &scope).await?;
        let reservation =
            page_support::reserve(database, &request, chrono::Utc::now().timestamp_millis())
                .await?;
        let capture = Capture {
            url: target.url,
            status: 200,
            content_type: "application/json".into(),
            body: serde_json::to_vec(&parsed)?,
        };
        let captured = CaptureRef {
            body: artifact(&capture.body)?,
            url: capture.url.clone(),
            status: capture.status,
            content_type: capture.content_type.clone(),
            retained_at_ms: 1_795_219_200_000,
        };
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
        Ok(Self {
            reservation,
            capture,
            captured,
            parsed,
            parsed_ref,
        })
    }

    async fn persist(&self, database: &Database) -> Result<PageOutcome> {
        for attempt in 0..3 {
            match database
                .durable_persist(
                    &self.reservation,
                    &self.captured,
                    &self.parsed_ref,
                    &self.capture,
                    &self.parsed,
                )
                .await
            {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    let retryable = error
                        .chain()
                        .filter_map(|cause| cause.downcast_ref::<sqlx::Error>())
                        .filter_map(sqlx::Error::as_database_error)
                        .any(|error| matches!(error.code().as_deref(), Some("40P01" | "55P03")));
                    if !retryable || attempt == 2 {
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        anyhow::bail!("identity persistence exhausted its bounded retry count")
    }
}

fn artifact(bytes: &[u8]) -> Result<ArtifactRef> {
    Ok(ArtifactRef {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        bytes: u64::try_from(bytes.len())?,
    })
}

async fn interleave(
    database: &Database,
    pool: &PgPool,
    first: &Prepared,
    second: &Prepared,
) -> Result<[PageOutcome; 2]> {
    sqlx::raw_sql("CREATE FUNCTION hold_identity_split() RETURNS trigger LANGUAGE plpgsql AS $$ \
        BEGIN IF NEW.needs_review THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318323); \
        END IF; RETURN NEW; END $$; CREATE TRIGGER hold_identity_split BEFORE INSERT ON athletes \
        FOR EACH ROW EXECUTE FUNCTION hold_identity_split();").execute(pool).await?;
    let mut gate = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),$1)")
        .bind(GATE)
        .execute(&mut *gate)
        .await?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let released = AtomicBool::new(false);
    let joined = tokio::time::timeout(Duration::from_secs(20), async {
        let later = async {
            let owner = waiting_writer(pool).await?;
            sender
                .send(owner)
                .map_err(|_| anyhow::anyhow!("split gate controller disappeared"))?;
            second.persist(database).await
        };
        let release = async {
            let ready = async {
                let owner = receiver.await.context("waiting for first split")?;
                waiting_on_owner(pool, owner).await
            }
            .await;
            unlock(&mut gate).await?;
            released.store(true, Ordering::Release);
            ready
        };
        tokio::join!(first.persist(database), later, release)
    })
    .await;
    let cleanup = if joined.is_err() && !released.load(Ordering::Acquire) {
        unlock(&mut gate).await
    } else {
        Ok(())
    };
    let closed = gate.close().await;
    cleanup?;
    closed.context("closing identity split gate connection")?;
    let (left, right, ready) = joined.context("identity split writers exceeded 20 seconds")?;
    ready?;
    let outcomes = [left?, right?];
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, PageOutcome::Completed { .. }))
    );
    Ok(outcomes)
}

async fn unlock(connection: &mut PgConnection) -> Result<()> {
    let unlocked: bool =
        sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext(current_schema()),$1)")
            .bind(GATE)
            .fetch_one(connection)
            .await?;
    ensure!(unlocked, "identity split gate was not held");
    Ok(())
}

async fn waiting_writer(pool: &PgPool) -> Result<i32> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let owner: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_locks WHERE locktype='advisory' \
                AND classid::bigint=(hashtext(current_schema())::bigint & 4294967295) \
                AND objid=$1::bigint AND NOT granted LIMIT 1",
            )
            .bind(i64::from(GATE))
            .fetch_optional(pool)
            .await?;
            if let Some(owner) = owner {
                return Ok(owner);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("first profile did not reach split insertion")?
}

async fn waiting_on_owner(pool: &PgPool, owner: i32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity \
                WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(owner)
            .fetch_one(pool)
            .await?;
            if waiting {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("second profile did not join the controlled split schedule")?
}
