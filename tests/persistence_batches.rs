#[path = "persistence_batches/support.rs"]
mod batching;
#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::Result;
use tracing::Instrument;
use tracing_subscriber::prelude::*;
use track_scrapper_v2::{
    database::Database,
    model::{ParsedPage, School, Source},
};

#[tokio::test]
async fn given_many_results_when_persisted_then_exact_references_are_loaded_in_bounded_queries_and_replay_keeps_ids()
-> Result<()> {
    let queries = batching::Queries::default();
    let subscriber = tracing_subscriber::registry().with(queries.clone());
    tracing::subscriber::set_global_default(subscriber)?;
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = records::target(Source::MileSplit, "11");
            batching::profiles(&database, &target).await?;
            let page = batching::results(257);
            let measured = records::persist(&database, &target, &page)
                .instrument(tracing::info_span!("persistence_query_observation"));
            let unrelated = sqlx::query("SELECT source_id,id FROM athlete_sources WHERE false")
                .execute(&test.pool);
            let (persisted, unrelated_result) = tokio::join!(measured, unrelated);
            persisted?;
            unrelated_result?;
            assert_eq!(queries.count("SELECT source_id,id FROM athlete_sources"), 3);
            assert_eq!(queries.count("SELECT source_id,id FROM schools"), 3);
            assert_eq!(queries.count("SELECT id FROM athlete_sources"), 0);
            assert_eq!(queries.count("SELECT id FROM schools"), 0);
            eprintln!(
                "257 results: profile reference queries={}, school reference queries={}",
                queries.count("SELECT source_id,id FROM athlete_sources"),
                queries.count("SELECT source_id,id FROM schools")
            );
            let rows = batching::result_ids(&test.pool).await?;
            assert_eq!(rows.len(), 257);
            assert!(
                rows.iter()
                    .enumerate()
                    .all(|(i, row)| row.1 == format!("batch-{i}")
                        && row.2 == if i % 2 == 0 { "11" } else { "22" }
                        && row.3 == if i % 2 == 0 { "101" } else { "102" }
                        && row.4)
            );
            records::persist(&database, &target, &page).await?;
            assert_eq!(batching::result_ids(&test.pool).await?, rows);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_an_explicit_cross_source_identity_when_the_current_page_separates_it_then_batched_results_use_the_new_canonical_owner()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let target=records::target(Source::MileSplit,"11");
        batching::profiles(&database,&target).await?;
        let mut linked=records::athlete("33");
        linked.same_as=vec![target.url.clone()];
        records::persist(&database,&records::target(Source::AthleticNet,"33"),&ParsedPage {athletes:vec![linked],..Default::default()}).await?;
        let shared: bool=sqlx::query_scalar("SELECT a.athlete_id=b.athlete_id FROM athlete_sources a CROSS JOIN athlete_sources b WHERE a.source='milesplit' AND a.source_id='11' AND b.source='athleticnet' AND b.source_id='33'").fetch_one(&test.pool).await?;
        assert!(shared);
        let mut page=batching::results(129);
        let mut corrected=records::athlete("11");
        corrected.name="Different Published Identity".into();
        corrected.school_id=Some("101".into());
        page.athletes=vec![corrected];
        records::persist(&database,&target,&page).await?;
        let separated: bool=sqlx::query_scalar("SELECT a.athlete_id<>b.athlete_id FROM athlete_sources a CROSS JOIN athlete_sources b WHERE a.source='milesplit' AND a.source_id='11' AND b.source='athleticnet' AND b.source_id='33'").fetch_one(&test.pool).await?;
        assert!(separated);
        let rows=batching::result_ids(&test.pool).await?;
        assert_eq!(rows.len(),129);
        assert!(rows.iter().all(|row|row.4));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM athlete_sources").fetch_one(&test.pool).await?,3);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_missing_references_and_bad_wind_when_batching_then_first_row_error_priority_and_atomic_rollback_are_preserved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = records::target(Source::MileSplit, "11");
            batching::profiles(&database, &target).await?;
            let mut page = batching::results(2);
            page.performances[0].school_id = Some("missing-school".into());
            page.performances[1].wind = Some(f64::NAN);
            let error = records::persist(&database, &target, &page)
                .await
                .expect_err("first unknown school wins");
            assert!(format!("{error:#}").contains("unknown milesplit school ID missing-school"));
            page.performances[0].athlete_id = "missing-athlete".into();
            let error = records::persist(&database, &target, &page)
                .await
                .expect_err("athlete precedes school");
            assert!(format!("{error:#}").contains("unknown milesplit athlete ID missing-athlete"));
            page.performances[0].wind = Some(f64::INFINITY);
            let error = records::persist(&database, &target, &page)
                .await
                .expect_err("wind precedes references");
            assert!(
                format!("{error:#}")
                    .contains("non-finite wind for milesplit athlete missing-athlete")
            );
            assert!(batching::result_ids(&test.pool).await?.is_empty());
            page = batching::results(129);
            page.performances[128].school_id = Some("missing-last-school".into());
            let error = records::persist(&database, &target, &page)
                .await
                .expect_err("later chunk rolls back earlier results");
            assert!(format!("{error:#}").contains("missing-last-school"));
            assert!(batching::result_ids(&test.pool).await?.is_empty());
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_duplicate_targets_and_saved_stops_when_queue_is_batched_then_serial_order_sequence_and_checkpoints_are_identical()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        batching::install_statement_counter(&test.pool).await?;
        let targets = batching::targets();
        batching::seed_stop(&database, &test.pool, &targets[0]).await?;
        let mut serial_count = 0;
        for target in &targets { serial_count += database.enqueue(std::slice::from_ref(target), false).await?; }
        let serial = batching::queue_snapshot(&test.pool).await?;
        let serial_sequence = batching::sequence(&test.pool).await?;
        assert_eq!(batching::statements(&test.pool).await?, i64::try_from(targets.len()+1)?);
        sqlx::raw_sql("TRUNCATE scrape_pages RESTART IDENTITY CASCADE; UPDATE batch_statements SET calls=0;")
            .execute(&test.pool).await?;
        batching::seed_stop(&database, &test.pool, &targets[0]).await?;
        let batched_count = database.enqueue(&targets, false).await?;
        assert_eq!(batched_count, serial_count);
        assert_eq!(batching::queue_snapshot(&test.pool).await?, serial);
        assert_eq!(batching::sequence(&test.pool).await?, serial_sequence);
        let statements = batching::statements(&test.pool).await?;
        eprintln!("queue INSERT statements: serial={}, batched={}, targets={}",targets.len()+1,statements,targets.len());
        assert!((3..=8).contains(&statements), "actual PostgreSQL INSERT statement count {statements}");
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_contact_aliases_and_refresh_when_enqueued_then_contact_suppression_and_refresh_policies_remain_sequential()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        batching::install_statement_counter(&test.pool).await?;
        let first = batching::contact("WI");
        let second = batching::contact("MI");
        assert_eq!(database.enqueue(&[first.clone(), second], false).await?, 1);
        assert_eq!(batching::statements(&test.pool).await?, 2);
        let target = batching::team(700);
        assert_eq!(database.enqueue(std::slice::from_ref(&target), false).await?, 1);
        sqlx::query("UPDATE scrape_pages SET status='succeeded',attempts=3,last_error='retained' WHERE url=$1")
            .bind(&target.url).execute(&test.pool).await?;
        assert_eq!(database.enqueue(&[target.clone(),target], true).await?, 1);
        let state: (String,i32,Option<String>) = sqlx::query_as("SELECT status,attempts,last_error FROM scrape_pages WHERE cursor->>'kind'='team'")
            .fetch_one(&test.pool).await?;
        assert_eq!(state, ("pending".into(),0,None));
        assert_eq!(batching::statements(&test.pool).await?, 5);
        assert_eq!(database.enqueue(&[], false).await?, 0);
        assert_eq!(batching::statements(&test.pool).await?, 5);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_a_repeated_school_conflict_when_reviewed_then_athlete_is_flagged_once_without_rewriting_true_flag()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        sqlx::raw_sql("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','77','School','MI','https://mi.milesplit.com/teams/77'),('athleticnet','88','Other','WI','https://www.athletic.net/Team/88'); INSERT INTO athletes(name) VALUES('Same Runner'); INSERT INTO athlete_sources(athlete_id,source,source_id,name,school_id) SELECT a.id,s.source,s.source_id,a.name,s.id FROM athletes a CROSS JOIN schools s;")
            .execute(&test.pool).await?;
        let target = batching::team(77);
        let page = ParsedPage { schools: vec![School { id:"77".into(),name:"School".into(),state:Some("MI".into()),..Default::default() }],..Default::default() };
        records::persist(&database,&target,&page).await?;
        let before: (String,bool) = sqlx::query_as("SELECT ctid::text,needs_review FROM athletes").fetch_one(&test.pool).await?;
        assert!(before.1);
        records::persist(&database,&target,&page).await?;
        let after: (String,bool) = sqlx::query_as("SELECT ctid::text,needs_review FROM athletes").fetch_one(&test.pool).await?;
        assert_eq!(after,before);
        database.close().await;
        Ok(())
    })).await
}
