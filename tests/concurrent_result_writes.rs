#[path = "concurrent_result_writes/support.rs"]
mod concurrency;
#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Sport, Target, UnassignedResult},
};

use concurrency::{Prepared, SparseCase};

#[tokio::test]
async fn given_missing_unassigned_result_when_sparse_insert_loses_then_normalization_is_preserved()
-> Result<()> {
    sparse_scenario(SparseCase::MissingMark).await
}

#[tokio::test]
async fn given_missing_unassigned_result_when_sparse_event_changes_then_units_are_recomputed()
-> Result<()> {
    sparse_scenario(SparseCase::ChangedEvent).await
}

#[tokio::test]
async fn given_missing_unassigned_result_when_sparse_dq_wins_then_numeric_mark_is_cleared()
-> Result<()> {
    sparse_scenario(SparseCase::Disqualified).await
}

async fn sparse_scenario(case: SparseCase) -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let sparse = Prepared::athleticnet(&database, "MI", case.event(), "", case.status())
                .await?;
            let complete =
                Prepared::athleticnet(&database, "MN", "100m", "12.34", None).await?;
            let (completed_first, outcomes) = concurrency::interleave(
                &database,
                &test.pool,
                &sparse,
                &complete,
                concurrency::SPARSE_GATE,
            )
            .await?;
            assert!(completed_first, "the complete insert must win the absent key");
            let stored: Value = sqlx::query_scalar(
                "SELECT jsonb_build_object('event',event,'mark',raw_mark,'numeric',normalized_numeric, \
                 'unit',normalized_unit,'status',status,'captured_mark',raw#>>'{raw,Result}', \
                 'raw_mark',raw->>'mark','raw_event',raw->>'event') FROM results",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(stored, case.expected());
            concurrency::assert_replay(&database, [&sparse, &complete], &outcomes).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_assigned_sparse_result_when_unassigned_insert_wins_then_identity_and_mark_survive()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let sparse = Prepared::assigned_sparse(&database).await?;
            let complete = Prepared::athleticnet(&database, "MN", "100m", "12.34", None).await?;
            let (completed_first, outcomes) = concurrency::interleave(
                &database,
                &test.pool,
                &sparse,
                &complete,
                concurrency::SPARSE_GATE,
            )
            .await?;
            assert!(completed_first);
            let stored: Value = sqlx::query_scalar(
                "SELECT jsonb_build_object('event',r.event,'mark',r.raw_mark, \
                 'numeric',r.normalized_numeric,'unit',r.normalized_unit,'status',r.status, \
                 'profile',p.source_id,'owner_matches',r.athlete_id=p.athlete_id, \
                 'raw_event',r.raw->>'event','raw_mark',r.raw->>'mark') \
                 FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id",
            )
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                stored,
                json!({"event":"100m","mark":"12.34",
                "numeric":12_340_000_000_i64,"unit":"ns","status":"recorded",
                "profile":"424242","owner_matches":true,"raw_event":"100m","raw_mark":"12.34"})
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM results")
                    .fetch_one(&test.pool)
                    .await?,
                1
            );
            concurrency::assert_replay(&database, [&sparse, &complete], &outcomes).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_file_aliases_when_revisions_overlap_then_only_the_last_capture_remains_current()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let first = Prepared::milesplit(&database, "oh", "12:40.6").await?;
            let second = Prepared::milesplit(&database, "www", "12:39.0").await?;
            assert_ne!(first.page_id(), second.page_id());
            let (_, outcomes) = concurrency::interleave(
                &database,
                &test.pool,
                &first,
                &second,
                concurrency::REVISION_GATE,
            )
            .await?;
            let rows: Vec<Value> = sqlx::query_scalar(
                "SELECT jsonb_build_object('mark',raw_mark,'revision',raw#>>'{raw,revision_status}', \
                 'assigned',athlete_source_id IS NOT NULL) FROM results ORDER BY raw_mark",
            )
            .fetch_all(&test.pool)
            .await?;
            assert_eq!(
                rows,
                [
                    json!({"mark":"12:39.0","revision":null,"assigned":false}),
                    json!({"mark":"12:40.6","revision":"unverified_prior_revision","assigned":false})
                ]
            );
            concurrency::assert_replay(&database, [&first, &second], &outcomes).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_more_than_64_published_files_when_persisted_then_the_page_is_rejected_atomically()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(Source::MileSplit, PageKind::Results,
                "https://mi.milesplit.com/meets/101/results/1/raw".into(), PageContext::default());
            let capture = Capture { url: target.url.clone(), status: 200,
                content_type: "text/html".into(), body: Vec::new() };
            let page = ParsedPage {
                unassigned_results: (1..=65).map(|file| UnassignedResult {
                    result_id: Some(format!("file-{file}")), source_row: None,
                    event: "100m".into(), mark: "12.34".into(), sport: Sport::Outdoor,
                    school_id: None, meet_id: Some("101".into()), date: None,
                    is_relay: false, members: Vec::new(),
                    raw: json!({"kind":"raw_result", "published_file_identity":format!("milesplit-file:101:{file}")}),
                }).collect(),
                ..Default::default()
            };
            let error = database.persist(None, &target, &capture, &page).await
                .expect_err("the page exceeds its published-file lock grant");
            assert!(error.to_string().contains("exceeds 64 files per page"), "{error:#}");
            let counts: (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM results),(SELECT count(*) FROM scrape_pages)",
            ).fetch_one(&test.pool).await?;
            assert_eq!(counts, (0, 0));
            database.close().await;
            Ok(())
        })
    }).await
}
