use anyhow::Result;
use track_scrapper_v2::{
    config::Config,
    durable::model::{CrawlPhase, CrawlRequest, delay_until, digest},
};

fn request() -> Result<CrawlRequest> {
    Ok(CrawlRequest {
        inherited_retry_campaigns: Vec::new(),
        scheduler_version: None,
        deadline_extension: None,
        generation: "wisconsin-recovery".into(),
        config: Config::parse(
            "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\nprofile_coverage_only=true\n[sources.athleticnet]\nenabled=false",
        )?,
        deadline_ms: 120_000,
        cooldown_ms: 15_000,
        batch: 0,
        max_batches: 100,
        phase: CrawlPhase::SchoolChains,
    })
}

#[test]
fn given_a_phase_transition_when_config_changes_then_scope_identity_changes_and_original_stays_bound()
-> Result<()> {
    let original = request()?;
    original.validate(10_000)?;
    let before = original.page_scope()?;
    let next = original.next(CrawlPhase::PublicResults)?;
    assert_eq!(original.workflow_key(), "wisconsin-recovery-000000");
    assert_eq!(next.workflow_key(), "wisconsin-recovery-000001");
    assert_eq!(before.config_sha256, digest(&original.config)?);
    assert_ne!(before.run_generation, next.page_scope()?.run_generation);
    assert_ne!(before.config_sha256, next.page_scope()?.config_sha256);
    assert_eq!(before, original.page_scope()?);
    Ok(())
}

#[test]
fn given_unbounded_or_ambiguous_commands_when_validated_then_they_reject_before_execution()
-> Result<()> {
    let original = request()?;
    let mut invalid = original.clone();
    invalid.generation = "../escape".into();
    assert!(invalid.validate(10_000).is_err());
    invalid = original.clone();
    invalid.deadline_ms = 86_410_001;
    assert!(invalid.validate(10_000).is_err());
    invalid = original.clone();
    invalid.batch = invalid.max_batches;
    assert!(invalid.validate(10_000).is_err());
    invalid = original.clone();
    invalid.config.states = vec!["wi".into()];
    assert!(invalid.validate(10_000).is_err());
    invalid = original;
    invalid.cooldown_ms = 0;
    assert!(invalid.validate(10_000).is_err());
    assert_eq!(delay_until(100, 50)?.as_millis(), 0);
    assert_eq!(delay_until(100, 250)?.as_millis(), 150);
    Ok(())
}

#[tokio::test]
async fn given_nonlocal_ingress_when_submitting_then_no_external_request_can_be_sent() {
    for ingress in [
        "https://example.org",
        "http://127.0.0.1.evil.invalid",
        "http://user:secret@127.0.0.1:18095",
        "http://127.0.0.1:18095/path",
    ] {
        let result = track_scrapper_v2::durable::cli::send(
            ingress,
            "restate/send/test/run",
            Some("test"),
            Some(&()),
        )
        .await;
        assert!(
            result
                .err()
                .expect("invalid ingress must reject")
                .to_string()
                .contains("loopback")
        );
    }
}

#[derive(serde::Serialize)]
struct HistoricalRequest<'a> {
    generation: &'a str,
    config: &'a Config,
    deadline_ms: i64,
    cooldown_ms: i64,
    batch: u32,
    max_batches: u32,
    phase: CrawlPhase,
}

#[test]
fn omitted_scheduler_keeps_exact_historical_request_bytes_and_page_scope_identity() -> Result<()> {
    let request = request()?;
    let old = HistoricalRequest {
        generation: &request.generation,
        config: &request.config,
        deadline_ms: request.deadline_ms,
        cooldown_ms: request.cooldown_ms,
        batch: request.batch,
        max_batches: request.max_batches,
        phase: request.phase,
    };
    assert_eq!(
        serde_json::to_string(&request)?,
        serde_json::to_string(&old)?
    );
    assert_eq!(digest(&request)?, digest(&old)?);
    let upgraded = request.clone().upgraded_scheduler();
    assert_eq!(request.page_scope()?, upgraded.page_scope()?);
    assert_eq!(request.deadline_ms, upgraded.deadline_ms);
    assert_eq!(
        request.inherited_retry_campaigns,
        upgraded.inherited_retry_campaigns
    );
    assert_eq!(
        upgraded.scheduler_version,
        Some(track_scrapper_v2::durable::model::SchedulerVersion::RollingV2)
    );
    Ok(())
}

#[test]
fn scheduler_upgrade_changes_only_the_unrecorded_continuation_version_field() -> Result<()> {
    let original = request()?;
    let prior_bytes = serde_json::to_string(&original)?;
    let expected = original.next(CrawlPhase::PublicResults)?;
    let upgraded = expected.clone().upgraded_scheduler();
    let mut json = serde_json::to_value(&upgraded)?;
    assert_eq!(json["scheduler_version"], "rolling_v2");
    json.as_object_mut()
        .expect("request object")
        .remove("scheduler_version");
    assert_eq!(json, serde_json::to_value(&expected)?);
    assert_eq!(serde_json::to_string(&original)?, prior_bytes);
    json["scheduler_version"] = serde_json::json!("unqualified_future_mode");
    assert!(serde_json::from_value::<CrawlRequest>(json).is_err());
    Ok(())
}
