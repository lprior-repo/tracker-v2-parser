mod support;

use std::{pin::pin, time::Duration};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sqlx::PgPool;
use track_scrapper_v2::{
    database::{Database, Job},
    model::{PageContext, PageKind, Source, Target},
};

const FIRST_URL: &str = "https://first-school.example.edu/staff";
const SECOND_URL: &str = "https://second-school.example.edu/staff";
const TEAM_URL: &str = "https://wi.milesplit.com/teams/401";
const WAIT_LIMIT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn given_concurrent_contact_alias_discoveries_when_enqueued_then_one_immutable_checkpoint_survives()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let first = contact(FIRST_URL, "401", 2026);
            let alias = contact(FIRST_URL, "401", 2027);
            let counts = enqueue_overlapping(
                &database,
                &test.pool,
                std::slice::from_ref(&first),
                std::slice::from_ref(&alias),
            )
            .await?;
            assert_eq!(counts, (1, 0));
            let mut rows = checkpoints(&test.pool).await?;
            assert_eq!(rows.len(), 1);
            let (id, url, cursor) = rows.pop().context("retained contact checkpoint")?;
            assert_eq!(
                (url.as_str(), cursor),
                (FIRST_URL, serde_json::to_value(&first)?)
            );

            assert!(database.start_attempt(id, 3).await?);
            database
                .fail_job(&Job { id, target: first }, None, "retained failure", false)
                .await?;
            let before = checkpoint_state(&test.pool, id).await?;
            assert_eq!(database.enqueue(&[alias], true).await?, 0);
            assert_eq!(checkpoint_state(&test.pool, id).await?, before);
            assert_eq!(before["status"], "failed");
            assert_eq!(before["attempts"], 1);
            assert_eq!(before["last_error"], "retained failure");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_reversed_contact_alias_batches_when_enqueued_concurrently_then_each_alias_is_retained_once_without_deadlock()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let left = [
                contact(FIRST_URL, "401", 2026),
                contact(SECOND_URL, "402", 2026),
            ];
            let right = [
                Target::get(
                    Source::MileSplit,
                    PageKind::Team,
                    TEAM_URL.into(),
                    PageContext::default(),
                ),
                contact(SECOND_URL, "402", 2027),
                contact(FIRST_URL, "401", 2027),
            ];
            assert_eq!(
                enqueue_overlapping(&database, &test.pool, &left, &right).await?,
                (2, 1)
            );
            let actual = checkpoints(&test.pool)
                .await?
                .into_iter()
                .map(|(_, url, cursor)| (url, cursor))
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                [
                    (FIRST_URL.to_owned(), serde_json::to_value(&left[0])?),
                    (SECOND_URL.to_owned(), serde_json::to_value(&left[1])?),
                    (TEAM_URL.to_owned(), serde_json::to_value(&right[0])?),
                ]
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn contact(url: &str, school_id: &str, season: i32) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        url.into(),
        PageContext {
            school_id: Some(school_id.into()),
            school_name: Some(format!("School {school_id}")),
            state: Some("WI".into()),
            season: Some(season),
            subject_id: Some("contacts-depth:0".into()),
            ..PageContext::default()
        },
    )
}

async fn checkpoints(pool: &PgPool) -> Result<Vec<(i64, String, Value)>> {
    Ok(
        sqlx::query_as("SELECT id,url,cursor FROM scrape_pages ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

async fn checkpoint_state(pool: &PgPool, id: i64) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p WHERE p.id=$1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

async fn enqueue_overlapping(
    database: &Database,
    pool: &PgPool,
    first: &[Target],
    second: &[Target],
) -> Result<(u64, u64)> {
    sqlx::raw_sql(
        "CREATE FUNCTION hold_first_contact_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.url='https://first-school.example.edu/staff' THEN
             PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318453);
           END IF;
           RETURN NEW;
         END $$;
         CREATE TRIGGER hold_first_contact_insert BEFORE INSERT ON scrape_pages
         FOR EACH ROW EXECUTE FUNCTION hold_first_contact_insert();",
    )
    .execute(pool)
    .await?;
    let mut owner = pool.begin().await?;
    let owner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *owner)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext(current_schema()),7318453)")
        .execute(&mut *owner)
        .await?;

    let mut left = pin!(database.enqueue(first, false));
    let first_pid = tokio::select! {
        result = &mut left => bail!("first enqueue did not reach the held insert: {result:?}"),
        ready = wait_for_blocked(pool, owner_pid, 0) => ready?,
    };
    let mut right = pin!(database.enqueue(second, false));
    tokio::select! {
        result = &mut right => bail!("second enqueue completed before the first insert: {result:?}"),
        ready = wait_for_blocked(pool, owner_pid, first_pid) => { ready?; },
    }
    owner.commit().await?;
    tokio::time::timeout(WAIT_LIMIT, async { tokio::try_join!(left, right) })
        .await
        .context("both overlapping enqueues must finish after the fixture lock is released")?
}

async fn wait_for_blocked(pool: &PgPool, owner: i32, first: i32) -> Result<i32> {
    tokio::time::timeout(WAIT_LIMIT, async {
        loop {
            let blocked: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity WHERE pid<>$2
                 AND ($1=ANY(pg_blocking_pids(pid)) OR $2=ANY(pg_blocking_pids(pid)))",
            )
            .bind(owner)
            .bind(first)
            .fetch_optional(pool)
            .await?;
            if let Some(pid) = blocked {
                return Ok(pid);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| {
        format!("enqueue did not block behind PostgreSQL backend {owner} or {first}")
    })?
}
