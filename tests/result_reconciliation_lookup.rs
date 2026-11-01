mod support;

#[path = "result_reconciliation_lookup/support.rs"]
mod lookup;

use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::database::Database;

#[tokio::test]
async fn given_long_retained_and_new_identifiers_when_lookup_indexes_are_installed_then_rows_and_exact_matches_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            lookup::drop_indexes(&test.pool).await?;
            let long: String = sqlx::query_scalar(
                "SELECT string_agg(md5(i::text),'' ORDER BY i) FROM generate_series(1,1024) i",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(long.len(), 32768);
            let url = format!("https://mi.milesplit.com/athletes/99?retained={long}");
            lookup::insert(
                &test.pool,
                "retained",
                "milesplit",
                Some(&long),
                &url,
                lookup::raw("personal_record"),
            )
            .await?;
            lookup::insert(
                &test.pool,
                "null",
                "milesplit",
                None,
                "https://mi.milesplit.com/absent",
                json!({}),
            )
            .await?;
            let before = lookup::snapshot(&test.pool).await?;
            lookup::install(&test.pool).await?;
            assert_eq!(lookup::snapshot(&test.pool).await?, before);
            let ids = lookup::by_ids(&test.pool, &[long.clone(), "absent".into()]).await?;
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].1.as_deref(), Some(long.as_str()));
            assert_eq!(lookup::by_url(&test.pool, &url).await?.len(), 1);
            let newer = format!("{long}new");
            lookup::insert(
                &test.pool,
                "new",
                "milesplit",
                Some(&newer),
                &format!("{url}&new=1"),
                lookup::raw("career_result"),
            )
            .await?;
            assert_eq!(lookup::by_ids(&test.pool, &[newer]).await?.len(), 1);
            assert_eq!(lookup::snapshot(&test.pool).await?.len(), 3);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_actual_hash_collisions_when_id_and_url_queries_run_then_only_exact_source_and_text_match()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let left = "pr:181598";
            let right = "pr:22156";
            let hashes: (i32, i32) = sqlx::query_as("SELECT hashtext($1),hashtext($2)")
                .bind(left).bind(right).fetch_one(&test.pool).await?;
            assert_eq!(hashes.0, hashes.1);
            lookup::insert(&test.pool, "left", "milesplit", Some(left), left, lookup::raw("personal_record")).await?;
            lookup::insert(&test.pool, "right", "milesplit", Some(right), right, lookup::raw("career_result")).await?;
            lookup::insert(&test.pool, "other-provider", "athleticnet", Some(left), left, lookup::raw("personal_record")).await?;
            lookup::insert(&test.pool, "wrong-kind", "milesplit", Some("other"), left, lookup::raw("raw_result")).await?;
            let ids = lookup::by_ids(&test.pool, &[left.into(), "absent".into()]).await?;
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].1.as_deref(), Some(left));
            assert_eq!(lookup::by_url(&test.pool, left).await?, ids.iter().map(|row| row.0).collect::<Vec<_>>());
            let both = lookup::by_ids(&test.pool, &[left.into(), right.into()]).await?;
            assert_eq!(both.len(), 2);
            let mut connection = test.pool.acquire().await?;
            sqlx::query("SET enable_seqscan=off").execute(&mut *connection).await?;
            let plan: Value = sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT id FROM results WHERE source='milesplit' AND source_result_id=$1")
                .bind(left).fetch_one(&mut *connection).await?;
            assert!(lookup::has_index(&plan, lookup::ID_INDEX));
            assert!(lookup::rechecked_rows(&plan) >= 1.0);
            database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_many_retained_results_when_scalar_array_reconfirmation_runs_then_index_plan_replaces_relation_scan_without_changed_rows()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            lookup::drop_indexes(&test.pool).await?;
            lookup::seed(&test.pool).await?;
            let ids = vec!["pr:24999:outdoor:all:800m".to_owned(), "pr:25000:outdoor:all:800m".to_owned(), "absent".to_owned()];
            let before_rows = lookup::by_ids(&test.pool, &ids).await?;
            let before = lookup::explain_ids(&test.pool, &ids, false).await?;
            assert!(lookup::scans_results(&before));
            lookup::install(&test.pool).await?;
            let after = lookup::explain_ids(&test.pool, &ids, false).await?;
            let generic = lookup::explain_ids(&test.pool, &ids, true).await?;
            assert_eq!(lookup::by_ids(&test.pool, &ids).await?, before_rows);
            for plan in [&after, &generic] {
                assert!(lookup::has_index(plan, lookup::ID_INDEX));
                assert!(!lookup::scans_results(plan));
            }
            assert!(generic.to_string().contains("$1"));
            assert!(lookup::heap_blocks(&after) * 20 < lookup::heap_blocks(&before));
            assert_eq!(lookup::updated_rows(&after), 2.0);
            let unchanged: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE raw#>>'{source_values,mask_reconciliation}'='retained_mark_masked'")
                .fetch_one(&test.pool).await?;
            assert_eq!(unchanged, 50000);
            eprintln!("result-id baseline={}ms indexed={}ms", lookup::elapsed(&before)?, lookup::elapsed(&after)?);
            database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_many_profile_observations_when_exact_url_review_runs_then_index_preserves_sorted_limit_and_reduces_heap_reads()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            lookup::drop_indexes(&test.pool).await?;
            lookup::seed(&test.pool).await?;
            let url = "https://mi.milesplit.com/athletes/25000-runner";
            lookup::insert(
                &test.pool,
                "same-url",
                "milesplit",
                Some("extra"),
                url,
                lookup::raw("career_result"),
            )
            .await?;
            lookup::insert(
                &test.pool,
                "wrong-kind",
                "milesplit",
                Some("raw"),
                url,
                lookup::raw("raw_result"),
            )
            .await?;
            let expected = lookup::by_url(&test.pool, url).await?;
            assert_eq!(expected.len(), 2);
            assert!(expected.windows(2).all(|rows| rows[0] < rows[1]));
            let before = lookup::explain_url(&test.pool, url).await?;
            assert!(lookup::scans_results(&before));
            lookup::install(&test.pool).await?;
            let after = lookup::explain_url(&test.pool, url).await?;
            assert!(lookup::has_index(&after, lookup::URL_INDEX));
            assert!(!lookup::scans_results(&after));
            assert_eq!(lookup::by_url(&test.pool, url).await?, expected);
            assert!(lookup::heap_blocks(&after) * 20 < lookup::heap_blocks(&before));
            assert_eq!(
                lookup::by_url(&test.pool, "https://mi.milesplit.com/athletes/missing").await?,
                Vec::<i64>::new()
            );
            eprintln!(
                "profile-url baseline={}ms indexed={}ms",
                lookup::elapsed(&before)?,
                lookup::elapsed(&after)?
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}
