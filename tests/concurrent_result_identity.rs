#[path = "support/records.rs"]
mod records;
mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;
use track_scrapper_v2::{
    database::Database,
    model::{ParsedPage, School, Source},
};

const GATE: i32 = 7318324;

#[tokio::test]
async fn given_linked_profiles_when_a_single_result_waits_for_a_split_then_its_owner_matches_the_profile()
-> Result<()> {
    scenario(result_only_page(1)).await
}

#[tokio::test]
async fn given_linked_profiles_when_batched_results_wait_for_a_split_then_every_owner_matches_the_profile()
-> Result<()> {
    scenario(result_only_page(8)).await
}

#[tokio::test]
async fn given_large_school_references_when_results_wait_for_a_split_then_every_owner_matches_the_profile()
-> Result<()> {
    scenario(large_reference_page()).await
}

async fn scenario(incoming: ParsedPage) -> Result<()> {
    support::with_schema(move |test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let original = seed_linked_profiles(&database).await?;
            interleave(&database, &test.pool, &incoming).await?;
            let (owner, other): (i64, i64) = sqlx::query_as(
                "SELECT p.athlete_id,other.athlete_id FROM athlete_sources p \
                 CROSS JOIN athlete_sources other WHERE p.source='athleticnet' AND p.source_id='11' \
                 AND other.source='milesplit' AND other.source_id='21'",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_ne!(owner, original);
            assert_eq!(other, original);
            let rows: Vec<(String, i64, i64, String)> = sqlx::query_as(
                "SELECT r.source_result_id,r.athlete_id,p.athlete_id,r.raw_mark FROM results r \
                 JOIN athlete_sources p ON p.id=r.athlete_source_id ORDER BY r.source_result_id",
            )
            .fetch_all(&test.pool)
            .await?;
            let mut expected = (0..incoming.performances.len())
                .map(|index| (format!("ownership-{index}"), owner, owner, "11.00".into()))
                .collect::<Vec<_>>();
            expected.sort_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(rows, expected);
            let schools: Vec<String> = sqlx::query_scalar("SELECT source_id FROM schools ORDER BY id")
                .fetch_all(&test.pool)
                .await?;
            assert_eq!(schools, incoming.schools.iter().map(|school| school.id.clone()).collect::<Vec<_>>());
            let checkpoints: i64 =
                sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE status='succeeded'")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(checkpoints, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}

fn profile(source: Source, id: &str, year: i32, linked: bool) -> ParsedPage {
    let mut athlete = records::athlete(id);
    athlete.graduation_year = Some(year);
    athlete.profile_url = Some(records::target(source, id).url);
    if linked {
        athlete.same_as = vec!["https://www.athletic.net/athlete/11".into()];
    }
    ParsedPage {
        athletes: vec![athlete],
        ..ParsedPage::default()
    }
}

async fn seed_linked_profiles(database: &Database) -> Result<i64> {
    records::persist(
        database,
        &records::target(Source::AthleticNet, "11"),
        &profile(Source::AthleticNet, "11", 2028, false),
    )
    .await?;
    records::persist(
        database,
        &records::target(Source::MileSplit, "21"),
        &profile(Source::MileSplit, "21", 2028, true),
    )
    .await?;
    let owners: Vec<i64> =
        sqlx::query_scalar("SELECT DISTINCT athlete_id FROM athlete_sources ORDER BY athlete_id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(owners.len(), 1);
    owners.first().copied().context("seeded canonical athlete")
}

fn result_only_page(count: usize) -> ParsedPage {
    ParsedPage {
        performances: (0..count)
            .map(|index| {
                let mut performance = records::performance("11.00");
                performance.result_id = Some(format!("ownership-{index}"));
                performance
            })
            .collect(),
        ..ParsedPage::default()
    }
}

fn large_reference_page() -> ParsedPage {
    let school_id = "1234567890".repeat(900);
    let mut page = result_only_page(128);
    for performance in &mut page.performances {
        performance.school_id = Some(school_id.clone());
    }
    page.schools.push(School {
        id: school_id,
        name: "Published school with a large source identity".into(),
        ..School::default()
    });
    page
}

async fn interleave(database: &Database, pool: &PgPool, incoming: &ParsedPage) -> Result<()> {
    sqlx::raw_sql(
        "CREATE FUNCTION hold_result_identity_split() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.needs_review THEN \
         PERFORM pg_advisory_xact_lock(hashtext(current_schema()),7318324); \
         END IF; RETURN NEW; END $$; \
         CREATE TRIGGER hold_result_identity_split BEFORE INSERT ON athletes \
         FOR EACH ROW EXECUTE FUNCTION hold_result_identity_split();",
    )
    .execute(pool)
    .await?;
    let mut gate = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext(current_schema()),$1)")
        .bind(GATE)
        .execute(&mut *gate)
        .await?;
    let target = records::target(Source::AthleticNet, "11");
    let split = profile(Source::AthleticNet, "11", 2029, false);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let joined = tokio::time::timeout(Duration::from_secs(20), async {
        let results = async {
            let owner = waiting_split(pool).await?;
            sender
                .send(owner)
                .map_err(|_| anyhow::anyhow!("result owner gate controller disappeared"))?;
            records::persist(database, &target, incoming).await
        };
        let release = async {
            let ready = async {
                let owner = receiver.await.context("waiting for profile split")?;
                waiting_on_owner(pool, owner).await
            }
            .await;
            gate.commit().await?;
            ready
        };
        tokio::join!(
            records::persist(database, &target, &split),
            results,
            release
        )
    })
    .await
    .context("result owner writers exceeded 20 seconds")?;
    joined.2?;
    joined.0?;
    joined.1?;
    Ok(())
}

async fn waiting_split(pool: &PgPool) -> Result<i32> {
    for _ in 0..500 {
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
    anyhow::bail!("profile did not reach the controlled split")
}

async fn waiting_on_owner(pool: &PgPool, owner: i32) -> Result<()> {
    for _ in 0..500 {
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
    anyhow::bail!("results did not join the controlled profile split")
}
