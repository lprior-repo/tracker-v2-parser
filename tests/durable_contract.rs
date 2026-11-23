use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    durable::model::{
        CrawlPhase, CrawlRequest, DeadlineExtensionAuthorization, delay_until, digest,
    },
};

fn request() -> Result<CrawlRequest> {
    CrawlRequest::new(
        "wisconsin-recovery".into(),
        Config::parse(
            "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\nprofile_coverage_only=true\n[sources.athleticnet]\nenabled=false",
        )?,
        120_000,
        15_000,
        100,
        CrawlPhase::SchoolChains,
        Vec::new(),
    )
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
    assert_eq!(before.config_sha256, digest(original.config())?);
    assert_ne!(before.run_generation, next.page_scope()?.run_generation);
    assert_ne!(before.config_sha256, next.page_scope()?.config_sha256);
    assert_eq!(before, original.page_scope()?);
    Ok(())
}

#[test]
fn given_invalid_static_bounds_when_raw_requests_are_decoded_then_each_is_rejected() -> Result<()> {
    let request = serde_json::to_value(request()?)?;
    for (field, value, error) in [
        (
            "generation",
            serde_json::json!(""),
            "identifier must contain",
        ),
        (
            "generation",
            serde_json::json!("../escape"),
            "identifier must contain",
        ),
        (
            "generation",
            serde_json::json!("a".repeat(101)),
            "crawl generation exceeds 100 bytes",
        ),
        (
            "max_batches",
            serde_json::json!(0),
            "invalid bounded batch range",
        ),
        (
            "max_batches",
            serde_json::json!(10_001),
            "invalid bounded batch range",
        ),
        (
            "batch",
            serde_json::json!(100),
            "invalid bounded batch range",
        ),
        (
            "batch",
            serde_json::json!(101),
            "invalid bounded batch range",
        ),
        (
            "cooldown_ms",
            serde_json::json!(14_999),
            "cooldown must be 15 seconds to one hour",
        ),
        (
            "cooldown_ms",
            serde_json::json!(3_600_001),
            "cooldown must be 15 seconds to one hour",
        ),
        (
            "deadline_ms",
            serde_json::json!(0),
            "invalid crawl deadline",
        ),
        (
            "deadline_ms",
            serde_json::json!(-1),
            "invalid crawl deadline",
        ),
    ] {
        let mut raw = request.clone();
        raw[field] = value;
        let rejected = serde_json::from_value::<CrawlRequest>(raw)
            .err()
            .with_context(|| format!("accepted invalid {field}"))?;
        assert!(rejected.to_string().contains(error), "{field}: {rejected}");
    }
    Ok(())
}

#[test]
fn given_an_invalid_extension_when_the_request_is_decoded_then_its_authority_is_rejected()
-> Result<()> {
    let request = serde_json::to_value(request()?)?;
    for (previous, effective, earlier, error) in [
        (0, 120_000, vec![], "must increase the prior deadline"),
        (120_000, 120_000, vec![], "must increase the prior deadline"),
        (121_000, 120_000, vec![], "must increase the prior deadline"),
        (
            100_000,
            119_999,
            vec![],
            "must explicitly authorize the request deadline",
        ),
        (
            100_000,
            120_001,
            vec![],
            "must explicitly authorize the request deadline",
        ),
        (
            100_000,
            120_000,
            vec![0],
            "must be positive, unique, ordered",
        ),
        (
            100_000,
            120_000,
            vec![10_000, 10_000],
            "must be positive, unique, ordered",
        ),
        (
            100_000,
            120_000,
            vec![10_001, 10_000],
            "must be positive, unique, ordered",
        ),
        (
            100_000,
            120_000,
            vec![100_000],
            "must be positive, unique, ordered",
        ),
        (
            100_000,
            120_000,
            (1..=129_i64).collect(),
            "exceeds 128 earlier deadlines",
        ),
    ] {
        let mut raw = request.clone();
        raw["deadline_extension"] = serde_json::json!({
            "previous_deadline_ms": previous,
            "effective_deadline_ms": effective,
            "earlier_deadlines_ms": earlier,
        });
        let rejected = serde_json::from_value::<CrawlRequest>(raw)
            .err()
            .context("accepted invalid deadline extension")?;
        assert!(rejected.to_string().contains(error), "{rejected}");
    }
    Ok(())
}

#[test]
fn given_raw_constructor_bounds_when_a_request_is_built_then_static_invariants_hold() -> Result<()>
{
    let config = Config::parse("states=['IA']")?;
    for (generation, deadline, cooldown, maximum, error) in [
        ("bad/root", 1, 15_000, 1, "identifier must contain"),
        ("valid", 0, 15_000, 1, "invalid crawl deadline"),
        (
            "valid",
            1,
            14_999,
            1,
            "cooldown must be 15 seconds to one hour",
        ),
        ("valid", 1, 15_000, 0, "invalid bounded batch range"),
    ] {
        let rejected = CrawlRequest::new(
            generation.into(),
            config.clone(),
            deadline,
            cooldown,
            maximum,
            CrawlPhase::SchoolChains,
            Vec::new(),
        )
        .err()
        .context("accepted invalid constructor bounds")?;
        assert!(rejected.to_string().contains(error), "{rejected}");
    }
    Ok(())
}

#[test]
fn given_a_new_extension_when_attached_then_its_deadline_must_match_the_request() -> Result<()> {
    let authorization = DeadlineExtensionAuthorization {
        previous_deadline_ms: 100_000,
        effective_deadline_ms: 120_000,
        earlier_deadlines_ms: Vec::new(),
    };
    let mismatch = DeadlineExtensionAuthorization {
        effective_deadline_ms: 120_001,
        ..authorization.clone()
    };
    let rejected = request()?
        .with_deadline_extension(mismatch)
        .err()
        .context("accepted an extension for a different deadline")?;
    assert_eq!(
        rejected.to_string(),
        "crawl deadline extension must explicitly authorize the request deadline"
    );
    let extended = request()?.with_deadline_extension(authorization.clone())?;
    let raw = serde_json::to_value(extended)?;
    assert_eq!(
        raw["deadline_extension"],
        serde_json::json!({
            "previous_deadline_ms": 100_000,
            "effective_deadline_ms": 120_000,
        })
    );
    let restored: CrawlRequest = serde_json::from_value(raw)?;
    assert_eq!(restored.deadline_extension(), Some(&authorization));
    Ok(())
}

#[test]
fn given_an_expired_or_distant_deadline_when_read_then_only_workflow_validation_rejects()
-> Result<()> {
    let mut raw = serde_json::to_value(request()?)?;
    raw["deadline_ms"] = serde_json::json!(86_410_001);
    let restored: CrawlRequest = serde_json::from_value(raw)?;
    assert_eq!(restored.deadline_ms(), 86_410_001);
    let too_distant = restored
        .validate(10_000)
        .err()
        .context("accepted a deadline more than 24 hours away")?;
    assert_eq!(
        too_distant.to_string(),
        "crawl deadline must be within 24 hours"
    );
    restored.validate(10_001)?;
    restored.validate(restored.deadline_ms())?;
    let expired = restored
        .validate(86_410_002)
        .err()
        .context("accepted an expired deadline")?;
    assert_eq!(
        expired.to_string(),
        "crawl deadline must be within 24 hours"
    );
    assert_eq!(delay_until(100, 50)?.as_millis(), 0);
    assert_eq!(delay_until(100, 250)?.as_millis(), 150);
    Ok(())
}

#[test]
fn given_noncanonical_config_when_decoded_then_workflow_validation_does_not_normalize_it()
-> Result<()> {
    let mut raw = serde_json::to_value(request()?)?;
    raw["config"]["states"] = serde_json::json!(["wi"]);
    let restored: CrawlRequest = serde_json::from_value(raw)?;
    assert_eq!(restored.config().states, ["wi"]);
    let rejected = restored
        .validate(10_000)
        .err()
        .context("accepted a noncanonical configuration at workflow entry")?;
    assert_eq!(
        rejected.to_string(),
        "crawl configuration must be canonical"
    );
    Ok(())
}

#[test]
fn given_the_last_permitted_batch_when_advancing_then_no_invalid_request_is_created() -> Result<()>
{
    let mut raw = serde_json::to_value(request()?)?;
    raw["generation"] = serde_json::json!("a".repeat(100));
    raw["batch"] = serde_json::json!(9_998);
    raw["max_batches"] = serde_json::json!(10_000);
    raw["cooldown_ms"] = serde_json::json!(3_600_000);
    let penultimate: CrawlRequest = serde_json::from_value(raw)?;
    assert!(!penultimate.is_last_batch());
    let last = penultimate.next(CrawlPhase::PublicResults)?;
    assert!(last.is_last_batch());
    assert_eq!(last.batch(), 9_999);
    assert_eq!(last.max_batches(), 10_000);
    assert_eq!(last.cooldown_ms(), 3_600_000);
    assert_eq!(last.generation().len(), 100);
    let exhausted = last
        .next(CrawlPhase::PublicResults)
        .err()
        .context("advanced beyond the last bounded batch")?;
    assert_eq!(
        exhausted.to_string(),
        "bounded batch count reached; retained checkpoints are resumable"
    );
    assert_eq!(penultimate.batch(), 9_998);
    Ok(())
}

#[test]
fn given_a_complete_extended_wire_payload_when_decoded_then_exact_bytes_survive() -> Result<()> {
    let config = serde_json::to_string(&Config::parse("states=['IA']")?)?;
    let expected = format!(
        concat!(
            "{{\"generation\":\"historic\",\"config\":{},\"deadline_ms\":120000,",
            "\"cooldown_ms\":15000,\"batch\":2,\"max_batches\":3,\"phase\":\"public_results\",",
            "\"inherited_retry_campaigns\":[\"ancestor\"],\"scheduler_version\":\"rolling_v2\",",
            "\"deadline_extension\":{{\"previous_deadline_ms\":100000,",
            "\"effective_deadline_ms\":120000,\"earlier_deadlines_ms\":[80000,90000]}}}}"
        ),
        config
    );
    let restored: CrawlRequest = serde_json::from_str(&expected)?;
    restored.validate(10_000)?;
    assert_eq!(serde_json::to_string(&restored)?, expected);
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
        generation: request.generation(),
        config: request.config(),
        deadline_ms: request.deadline_ms(),
        cooldown_ms: request.cooldown_ms(),
        batch: request.batch(),
        max_batches: request.max_batches(),
        phase: request.phase(),
    };
    let expected = serde_json::to_string(&old)?;
    assert_eq!(serde_json::to_string(&request)?, expected);
    let decoded: CrawlRequest = serde_json::from_str(&expected)?;
    assert_eq!(serde_json::to_string(&decoded)?, expected);
    let mut with_defaults = serde_json::to_value(&old)?;
    with_defaults["inherited_retry_campaigns"] = serde_json::json!([]);
    with_defaults["scheduler_version"] = serde_json::Value::Null;
    with_defaults["deadline_extension"] = serde_json::Value::Null;
    let decoded_defaults: CrawlRequest = serde_json::from_value(with_defaults)?;
    assert_eq!(serde_json::to_string(&decoded_defaults)?, expected);
    assert_eq!(digest(&request)?, digest(&old)?);
    let upgraded = request.clone().upgraded_scheduler();
    assert_eq!(request.page_scope()?, upgraded.page_scope()?);
    assert_eq!(request.deadline_ms(), upgraded.deadline_ms());
    assert_eq!(
        request.inherited_retry_campaigns(),
        upgraded.inherited_retry_campaigns()
    );
    assert_eq!(
        upgraded.scheduler_version(),
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
