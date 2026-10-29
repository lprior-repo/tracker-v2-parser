#[path = "durable_page/support.rs"]
mod page_support;
mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::TransportMode,
    database::Database,
    durable::{
        model::{CrawlPhase, CrawlRequest, digest, page_config_digest},
        operations::OperationSpec,
        page,
    },
};

fn request() -> Result<CrawlRequest> {
    let mut config = page_support::config()?;
    config.export.layout = track_scrapper_v2::export::ExportLayout::SingleSheet;
    let scope = page_support::scope(&config)?;
    Ok(CrawlRequest {
        generation: "presentation-only-review-change".into(),
        config,
        deadline_ms: scope.deadline_ms,
        cooldown_ms: scope.cooldown_ms,
        batch: 0,
        max_batches: 100,
        phase: CrawlPhase::SchoolChains,
    })
}

#[test]
fn review_sheet_preserves_old_page_digest_but_changes_full_export_binding() -> Result<()> {
    let original = request()?;
    let original_scope = original.page_scope()?;
    assert!(!original.config.export.review_sheet);
    assert!(
        serde_json::to_value(&original.config)?["export"]
            .get("review_sheet")
            .is_none()
    );
    assert_eq!(
        page_config_digest(&original.config)?,
        digest(&original.config)?
    );
    let mut reviewed = original.clone();
    reviewed.batch = 1;
    reviewed.config.export.review_sheet = true;
    reviewed.validate(chrono::Utc::now().timestamp_millis())?;
    assert_eq!(original_scope, reviewed.page_scope()?);
    assert_ne!(digest(&original.config)?, digest(&reviewed.config)?);
    assert_ne!(
        digest(&OperationSpec::Export {
            config: original.config.clone()
        })?,
        digest(&OperationSpec::Export {
            config: reviewed.config.clone()
        })?
    );
    assert_ne!(original.workflow_key(), reviewed.workflow_key());
    let mut source = reviewed.clone();
    source.config.sources.milesplit.transport = TransportMode::Browser;
    let mut rps = reviewed.clone();
    rps.config.limits.requests_per_second = 2.0;
    let mut year = reviewed.clone();
    year.config.graduation_years = vec![2028];
    let mut state = reviewed.clone();
    state.config.states = vec!["WI".into()];
    let mut acquisition = reviewed.clone();
    acquisition.config.acquisition = Default::default();
    let mut other_presentation = reviewed.clone();
    other_presentation.config.export.rows_per_file += 1;
    for changed in [source, rps, year, state, acquisition, other_presentation] {
        assert_ne!(
            original_scope.config_sha256,
            changed.page_scope()?.config_sha256
        );
    }
    let mut noncanonical = reviewed;
    noncanonical.config.states = vec!["oh".into()];
    assert!(
        noncanonical
            .validate(chrono::Utc::now().timestamp_millis())
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn review_sheet_resume_reuses_exact_unfinished_reservation_without_allowing_target_or_acquisition_drift()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let original = request()?;
            let scope = original.page_scope()?;
            let target = page_support::target("901");
            let initial = page::bind_target(&database, target.clone(), &scope).await?;
            let now = chrono::Utc::now().timestamp_millis();
            let reserved = page_support::reserve(&database, &initial, now).await?;
            let before = page_support::snapshots(&database).await?;
            let mut reviewed = original.clone();
            reviewed.batch = 1;
            reviewed.config.export.review_sheet = true;
            let next_scope = reviewed.page_scope()?;
            assert_eq!(next_scope, scope);
            let next = page::select(&database, &reviewed.config, &next_scope, now)
                .await?
                .context("unfinished reservation remains selectable")?;
            assert_eq!(next.binding, initial.binding);
            let replayed = page_support::reserve(&database, &next, now).await?;
            assert_eq!(replayed.attempt_id, reserved.attempt_id);
            assert_eq!(replayed.total_attempts, 1);
            assert_eq!(page_support::snapshots(&database).await?, before);
            reviewed.config.graduation_years = vec![2028];
            assert!(
                page::select(&database, &reviewed.config, &next_scope, now)
                    .await
                    .is_err()
            );
            let changed_target = page::request(
                track_scrapper_v2::database::Job {
                    id: initial.binding.page_id,
                    target: page_support::target("902"),
                },
                &next_scope,
            )?;
            assert_ne!(changed_target.binding, initial.binding);
            assert!(
                page::reserve(&database, &changed_target, now)
                    .await
                    .is_err()
            );
            assert_eq!(page_support::snapshots(&database).await?, before);
            database.close().await;
            Ok(())
        })
    })
    .await
}
