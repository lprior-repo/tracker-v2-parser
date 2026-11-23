use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use track_scrapper_v2::{
    database::Database,
    durable::page::{ArtifactRef, CaptureRef, PageOutcome, PageReservation, ParsedRef},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target},
    sources,
};

const GATE: i32 = 7318322;
const RAW: &str = include_str!("../../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html");

pub const SPARSE_GATE: &str = "CREATE FUNCTION hold_result_insert() RETURNS trigger LANGUAGE plpgsql AS $$ \
    BEGIN IF NEW.raw_mark='' OR NEW.event='' THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318322); \
    END IF; RETURN NEW; END $$; CREATE TRIGGER hold_result_insert BEFORE INSERT ON results \
    FOR EACH ROW EXECUTE FUNCTION hold_result_insert();";

pub const REVISION_GATE: &str = "CREATE FUNCTION hold_result_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$ \
    BEGIN IF NEW.status='succeeded' THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318322); \
    END IF; RETURN NEW; END $$; CREATE TRIGGER hold_result_checkpoint BEFORE INSERT ON scrape_pages \
    FOR EACH ROW EXECUTE FUNCTION hold_result_checkpoint();";

pub const NAVIGATION_GATE: &str = "CREATE FUNCTION hold_profile_navigation() RETURNS trigger LANGUAGE plpgsql AS $$ \
    BEGIN IF NEW.status='pending' AND NEW.url='https://oh.milesplit.com/athletes/15005189-brice-fuller/stats' \
    THEN PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318322); \
    END IF; RETURN NEW; END $$; CREATE TRIGGER hold_profile_navigation BEFORE INSERT ON scrape_pages \
    FOR EACH ROW EXECUTE FUNCTION hold_profile_navigation();";

#[derive(Clone, Copy)]
pub enum SparseCase {
    MissingMark,
    ChangedEvent,
    Disqualified,
}

impl SparseCase {
    pub fn event(self) -> &'static str {
        match self {
            Self::ChangedEvent => "Long Jump",
            Self::MissingMark | Self::Disqualified => "100m",
        }
    }

    pub fn status(self) -> Option<&'static str> {
        matches!(self, Self::Disqualified).then_some("DQ")
    }

    pub fn expected(self) -> Value {
        let (numeric, unit, status) = match self {
            Self::MissingMark => (Some(12_340_000_000_i64), Some("ns"), "recorded"),
            Self::ChangedEvent => (Some(12_340_000_i64), Some("um"), "recorded"),
            Self::Disqualified => (None, None, "DQ"),
        };
        json!({"event":self.event(),"mark":"12.34","numeric":numeric,"unit":unit,
            "status":status,"captured_mark":"","raw_mark":"12.34","raw_event":self.event()})
    }
}

pub struct Prepared {
    reservation: PageReservation,
    capture: Capture,
    captured: CaptureRef,
    parsed: ParsedPage,
    parsed_ref: ParsedRef,
}

impl Prepared {
    pub fn page_id(&self) -> i64 {
        self.reservation.request.binding.page_id()
    }

    pub fn has_navigation_to(&self, other: &Self) -> Result<bool> {
        let expected = serde_json::to_value(&other.reservation.request.target)?;
        for target in &self.parsed.next {
            if serde_json::to_value(target)? == expected {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn milesplit_profile_navigation(database: &Database, section: &str) -> Result<Self> {
        let mut target = crate::page_support::profile();
        target.url.push_str(section);
        let body = String::from_utf8(crate::page_support::capture()?.body)?;
        let parsed = sources::parse(&target, &body)?;
        assert_eq!(parsed.athletes.len(), 1);
        assert!(!parsed.performances.is_empty());
        Self::prepare_page(database, target, body, "text/html", parsed).await
    }

    pub async fn athleticnet(
        database: &Database,
        state: &str,
        event: &str,
        mark: &str,
        status: Option<&str>,
    ) -> Result<Self> {
        let target = athleticnet_target(state);
        let body = json!({"flatEvents":[{"Event":event,"results":[{
            "IDResult":90001,"Result":mark,"statusCode":status
        }]}],"teams":[]});
        Self::prepare(database, target, body.to_string(), "application/json").await
    }

    pub async fn assigned_sparse(database: &Database) -> Result<Self> {
        let target = athleticnet_target("MI");
        let body = json!({"flatEvents":[{"Event":"100m","results":[{
            "IDResult":90001,"Result":"12.34","AthleteID":424242,
            "FirstName":"Concurrent","LastName":"Athlete"
        }]}],"teams":[]})
        .to_string();
        let mut parsed = sources::parse(&target, &body)?;
        assert_eq!(parsed.performances.len(), 1);
        assert_eq!(parsed.athletes.len(), 1);
        assert!(parsed.unassigned_results.is_empty());
        parsed
            .performances
            .first_mut()
            .context("named result")?
            .event
            .clear();
        Self::prepare_page(database, target, body, "application/json", parsed).await
    }

    pub async fn milesplit(database: &Database, host: &str, mark: &str) -> Result<Self> {
        let target = Target::get(
            Source::MileSplit,
            PageKind::Results,
            format!("https://{host}.milesplit.com/meets/770621/results/1321880/raw"),
            PageContext {
                sport: Some(Sport::CrossCountry),
                ..Default::default()
            },
        );
        let (prefix, remaining) = RAW.split_once("<pre>").context("raw fixture pre block")?;
        let (block, suffix) = remaining.split_once("</pre>").context("raw fixture end")?;
        let row = block.lines().take(5).collect::<Vec<_>>().join("\n");
        let body = format!(
            "{prefix}<pre>{}</pre>{suffix}",
            row.replace("12:40.6", mark)
        );
        Self::prepare(database, target, body, "text/html").await
    }

    async fn prepare(
        database: &Database,
        target: Target,
        body: String,
        content_type: &str,
    ) -> Result<Self> {
        let parsed = sources::parse(&target, &body)?;
        assert!(parsed.athletes.is_empty());
        assert!(parsed.schools.is_empty());
        assert!(parsed.performances.is_empty());
        assert_eq!(parsed.unassigned_results.len(), 1);
        Self::prepare_page(database, target, body, content_type, parsed).await
    }

    async fn prepare_page(
        database: &Database,
        target: Target,
        body: String,
        content_type: &str,
        parsed: ParsedPage,
    ) -> Result<Self> {
        let scope = crate::page_support::scope(&crate::page_support::config()?)?;
        let request = crate::page_support::request(database, &target, &scope).await?;
        let reservation =
            crate::page_support::reserve(database, &request, chrono::Utc::now().timestamp_millis())
                .await?;
        let capture = Capture {
            url: target.url,
            status: 200,
            content_type: content_type.into(),
            body: body.into_bytes(),
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

fn athleticnet_target(state: &str) -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Results,
        "https://www.athletic.net/api/v1/Meet/GetAllResultsData?meetId=101&sport=tf&rawResults=false&showTips=false".into(),
        PageContext {
            state: Some(state.into()),
            subject_id: Some("101".into()),
            sport: Some(Sport::Outdoor),
            season: Some(2026),
            ..Default::default()
        },
    )
}

fn artifact(bytes: &[u8]) -> Result<ArtifactRef> {
    Ok(ArtifactRef {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        bytes: u64::try_from(bytes.len())?,
    })
}

pub async fn interleave(
    database: &Database,
    pool: &PgPool,
    first: &Prepared,
    second: &Prepared,
    trigger: &str,
) -> Result<(bool, [PageOutcome; 2])> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(trigger))
        .execute(pool)
        .await?;
    let mut gate = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),$1)")
        .bind(GATE)
        .execute(&mut *gate)
        .await?;
    let complete = AtomicBool::new(false);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let later = async {
        let pid = waiting_writer(pool).await?;
        sender
            .send(pid)
            .map_err(|_| anyhow::anyhow!("gate controller disappeared"))?;
        let result = second.persist(database).await;
        complete.store(true, Ordering::Release);
        result
    };
    let gate_released = AtomicBool::new(false);
    let joined = tokio::time::timeout(Duration::from_secs(12), async {
        let release = async {
            let observed = async {
                let pid = receiver.await.context("waiting for first result writer")?;
                completion_or_blocked(pool, pid, &complete).await
            }
            .await;
            unlock(&mut gate).await?;
            gate_released.store(true, Ordering::Release);
            observed
        };
        tokio::join!(first.persist(database), later, release)
    })
    .await;
    let cleanup = if joined.is_err() && !gate_released.load(Ordering::Acquire) {
        unlock(&mut gate).await
    } else {
        Ok(())
    };
    let closed = gate.close().await;
    cleanup?;
    closed.context("closing result concurrency gate connection")?;
    let (left, right, released) =
        joined.context("concurrent result writers exceeded 12 seconds")?;
    let completed_first = released?;
    let outcomes = [left?, right?];
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, PageOutcome::Completed { .. }))
    );
    Ok((completed_first, outcomes))
}

async fn unlock(connection: &mut PgConnection) -> Result<()> {
    let released: bool =
        sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext(current_schema()),$1)")
            .bind(GATE)
            .fetch_one(connection)
            .await?;
    ensure!(released, "result concurrency gate was not held");
    Ok(())
}

async fn waiting_writer(pool: &PgPool) -> Result<i32> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_locks WHERE locktype='advisory' \
                 AND classid::bigint=(hashtext(current_schema())::bigint & 4294967295) \
                 AND objid=$1::bigint AND NOT granted LIMIT 1",
            )
            .bind(i64::from(GATE))
            .fetch_optional(pool)
            .await?;
            if let Some(pid) = pid {
                return Ok(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("first writer did not reach the result gate")?
}

async fn completion_or_blocked(pool: &PgPool, owner: i32, complete: &AtomicBool) -> Result<bool> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if complete.load(Ordering::Acquire) {
                return Ok(true);
            }
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(owner)
            .fetch_one(pool)
            .await?;
            if blocked {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("second writer neither completed nor waited for the first")?
}

pub async fn assert_replay(
    database: &Database,
    pages: [&Prepared; 2],
    outcomes: &[PageOutcome; 2],
) -> Result<()> {
    let before = crate::page_support::snapshots(database).await?;
    for (page, outcome) in pages.into_iter().zip(outcomes) {
        assert_eq!(page.persist(database).await?, *outcome);
    }
    assert_eq!(crate::page_support::snapshots(database).await?, before);
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM durable_page_receipts), \
         (SELECT count(*) FROM scrape_pages WHERE status='succeeded'), \
         (SELECT count(*) FROM durable_wire_requests)",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(counts, (2, 2, 0));
    Ok(())
}
