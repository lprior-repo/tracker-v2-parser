mod support;

#[path = "raw_reconciliation_lookup/support.rs"]
mod lookup;

use anyhow::Result;
use serde_json::json;
use track_scrapper_v2::database::Database;

#[tokio::test]
async fn given_oversized_and_null_retained_identities_when_indexes_install_then_every_row_and_exact_lookup_survives()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            lookup::remove_indexes(&test.pool).await?;
            let long: String = sqlx::query_scalar(
                "SELECT string_agg(md5(i::text),'' ORDER BY i) FROM generate_series(1,1024) i",
            )
            .fetch_one(&test.pool)
            .await?;
            let observed = lookup::insert(
                &test.pool,
                "observed",
                "milesplit",
                Some(&long),
                lookup::raw("source_values", "raw_result", &long),
            )
            .await?;
            let unassigned = lookup::insert(
                &test.pool,
                "unassigned",
                "milesplit",
                None,
                lookup::raw("raw", "raw_result", &long),
            )
            .await?;
            lookup::insert(
                &test.pool,
                "null",
                "milesplit",
                None,
                json!({"raw":{"kind":"raw_result","published_file_identity":null}}),
            )
            .await?;
            lookup::insert(
                &test.pool,
                "other",
                "athleticnet",
                Some(&long),
                lookup::raw("raw", "raw_result", &long),
            )
            .await?;
            let before = lookup::snapshot(&test.pool).await?;
            lookup::install(&test.pool).await?;
            assert_eq!(lookup::snapshot(&test.pool).await?, before);
            assert_eq!(
                lookup::file_rows(&test.pool, &long).await?,
                vec![observed, unassigned]
            );
            assert!(lookup::file_rows(&test.pool, "absent").await?.is_empty());
            let newer = format!("{long}new");
            let id = lookup::insert(
                &test.pool,
                "new",
                "milesplit",
                Some(&newer),
                lookup::raw("raw", "raw_result", &newer),
            )
            .await?;
            assert_eq!(lookup::file_rows(&test.pool, &newer).await?, vec![id]);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_colliding_files_and_overlapping_json_paths_when_lookup_runs_then_only_exact_source_and_kind_match_once()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let left="pr:181598";
        let right="pr:22156";
        let hashes:(i32,i32)=sqlx::query_as("SELECT hashtext($1),hashtext($2)").bind(left).bind(right).fetch_one(&test.pool).await?;
        assert_eq!(hashes.0,hashes.1);
        let one=lookup::insert(&test.pool,"one","milesplit",Some(left),lookup::raw("source_values","raw_result",left)).await?;
        let both=lookup::insert(&test.pool,"both","milesplit",Some(left),json!({"source_values":{"kind":"raw_result","published_file_identity":left},"raw":{"kind":"raw_result","published_file_identity":left}})).await?;
        lookup::insert(&test.pool,"collision","milesplit",Some(right),lookup::raw("raw","raw_result",right)).await?;
        lookup::insert(&test.pool,"source","athleticnet",Some(left),lookup::raw("raw","raw_result",left)).await?;
        lookup::insert(&test.pool,"kind","milesplit",Some(left),lookup::raw("source_values","career_result",left)).await?;
        assert_eq!(lookup::file_rows(&test.pool,left).await?,vec![one,both]);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_large_retained_meet_history_when_lookup_indexes_install_then_generic_and_custom_plans_avoid_full_result_scans_with_identical_selection()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            lookup::remove_indexes(&test.pool).await?;
            lookup::seed(&test.pool).await?;
            let legacy = lookup::insert(
                &test.pool,
                "legacy",
                "milesplit",
                Some("25000"),
                json!({"source_values":{"kind":"raw_result"}}),
            )
            .await?;
            let file = "milesplit-file:25000:1";
            let files = lookup::file_rows(&test.pool, file).await?;
            let meets = lookup::meet_rows(&test.pool, &["25000".into()]).await?;
            assert_eq!(meets, vec![legacy]);
            let old_file = lookup::explain(&test.pool, file, false, false).await?;
            let old_meet = lookup::explain(&test.pool, "25000", true, false).await?;
            assert!(lookup::scans_results(&old_file));
            assert!(lookup::scans_results(&old_meet));
            lookup::install(&test.pool).await?;
            assert_eq!(lookup::file_rows(&test.pool, file).await?, files);
            assert_eq!(
                lookup::meet_rows(&test.pool, &["25000".into()]).await?,
                meets
            );
            for (value, meet, old) in [(file, false, &old_file), ("25000", true, &old_meet)] {
                let after = lookup::explain(&test.pool, value, meet, false).await?;
                let generic = lookup::explain(&test.pool, value, meet, true).await?;
                for plan in [&after, &generic] {
                    assert!(!lookup::scans_results(plan));
                    if meet {
                        assert!(lookup::has_index(plan, lookup::MEET));
                    } else {
                        assert!(lookup::has_index(plan, lookup::OBSERVED));
                        assert!(lookup::has_index(plan, lookup::UNASSIGNED));
                    }
                }
                assert!(lookup::heap_blocks(&after) * 20 < lookup::heap_blocks(old));
                eprintln!(
                    "raw lookup meet={meet} baseline={}ms indexed={}ms",
                    lookup::elapsed(old)?,
                    lookup::elapsed(&after)?
                );
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}
