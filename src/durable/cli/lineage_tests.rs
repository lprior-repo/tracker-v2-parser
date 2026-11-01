use anyhow::Result;

use super::{commands, tests::overrides};
use crate::{
    config::Config,
    durable::{
        model::{CrawlPhase, CrawlRequest},
        page::PageScope,
    },
};

#[test]
fn given_repeatable_ancestor_flags_when_request_is_frozen_then_every_phase_retains_lineage()
-> Result<()> {
    let flags = overrides(&[
        "--inherit-retry-campaign",
        "old-mixed",
        "--inherit-retry-campaign",
        "old-reviewed",
    ])?;
    let request = commands::crawl_request(
        Config::parse("states=['IA']")?,
        "new-iowa".into(),
        flags,
        1000,
    )?;
    assert_eq!(
        request.inherited_retry_campaigns,
        ["old-mixed", "old-reviewed"]
    );
    let encoded = serde_json::to_vec(&request)?;
    let restored: CrawlRequest = serde_json::from_slice(&encoded)?;
    assert_eq!(serde_json::to_vec(&restored)?, encoded);
    for phase in [
        CrawlPhase::SchoolChains,
        CrawlPhase::ContactRefresh,
        CrawlPhase::PublicResults,
    ] {
        let next = restored.next(phase)?;
        assert_eq!(
            next.inherited_retry_campaigns,
            request.inherited_retry_campaigns
        );
        assert_eq!(
            next.page_scope()?.inherited_retry_campaigns,
            request.inherited_retry_campaigns
        );
    }
    Ok(())
}

#[test]
fn given_empty_lineage_when_old_request_and_scope_are_loaded_then_serialized_bytes_stay_identical()
-> Result<()> {
    let request = commands::crawl_request(
        Config::parse("states=['IA']")?,
        "unchanged".into(),
        overrides(&[])?,
        1000,
    )?;
    let config = serde_json::to_string(&request.config)?;
    let expected = format!(
        "{{\"generation\":\"unchanged\",\"config\":{config},\"deadline_ms\":86401000,\"cooldown_ms\":60000,\"batch\":0,\"max_batches\":10000,\"phase\":\"school_chains\"}}"
    );
    assert_eq!(serde_json::to_string(&request)?, expected);
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<CrawlRequest>(&expected)?)?,
        expected
    );
    let encoded = format!(
        "{{\"run_generation\":\"unchanged-schools\",\"config_sha256\":\"{}\",\"deadline_ms\":86401000,\"cooldown_ms\":60000}}",
        "a".repeat(64)
    );
    let scope: PageScope = serde_json::from_str(&encoded)?;
    assert!(scope.inherited_retry_campaigns.is_empty());
    assert_eq!(serde_json::to_string(&scope)?, encoded);
    let target = crate::model::Target::get(
        crate::model::Source::MileSplit,
        crate::model::PageKind::Team,
        "https://ia.milesplit.com/teams/7-example".into(),
        crate::model::PageContext::default(),
    );
    let target_digest = crate::durable::model::digest(&target)?;
    let request = crate::durable::page::request(crate::database::Job { id: 7, target }, &scope)?;
    let prior_binding_bytes = format!("[7,\"{target_digest}\",{encoded}]");
    assert_eq!(
        request.binding.operation_id,
        crate::durable::page::body_digest(prior_binding_bytes.as_bytes())
    );
    Ok(())
}

#[test]
fn given_invalid_ancestor_lists_when_crawl_is_bound_then_the_boundary_rejects_them() -> Result<()> {
    for ancestors in [
        vec!["same"],
        vec!["old", "old"],
        vec!["old-schools"],
        vec!["old-contacts"],
        vec!["old-results"],
        vec!["old-complete"],
        vec![""],
        vec!["bad/name"],
        vec!["a"; 9],
    ] {
        let mut request = commands::crawl_request(
            Config::parse("states=['IA']")?,
            "same".into(),
            overrides(&[])?,
            1000,
        )?;
        request.inherited_retry_campaigns = ancestors.into_iter().map(str::to_owned).collect();
        assert!(request.validate(1000).is_err());
        assert!(request.page_scope().is_err());
    }
    let mut request = commands::crawl_request(
        Config::parse("states=['IA']")?,
        "same".into(),
        overrides(&[])?,
        1000,
    )?;
    request.inherited_retry_campaigns = vec!["a".repeat(101)];
    assert!(request.validate(1000).is_err());
    request.inherited_retry_campaigns = (0..8).map(|index| format!("ancestor-{index}")).collect();
    request.validate(1000)?;
    request.page_scope()?;
    request.generation = "same-schools".into();
    assert!(request.validate(1000).is_err());
    Ok(())
}

#[test]
fn given_wisconsin_when_retry_lineage_is_requested_then_cli_scope_and_page_policy_fail_closed()
-> Result<()> {
    for states in ["['WI']", "['IA','WI']"] {
        let config = Config::parse(&format!("states={states}"))?;
        let mut request =
            commands::crawl_request(config.clone(), "wisconsin".into(), overrides(&[])?, 1000)?;
        request.validate(1000)?;
        let mut scope = request.page_scope()?;
        request.inherited_retry_campaigns = vec!["old-mixed".into()];
        assert!(request.validate(1000).is_err());
        assert!(request.page_scope().is_err());
        assert!(
            commands::crawl_request(
                config.clone(),
                "wisconsin".into(),
                overrides(&["--inherit-retry-campaign", "old-mixed"])?,
                1000
            )
            .is_err()
        );
        scope.inherited_retry_campaigns = vec!["old-mixed".into()];
        let target = crate::model::Target::get(
            crate::model::Source::MileSplit,
            crate::model::PageKind::Team,
            "https://wi.milesplit.com/teams/7-school".into(),
            crate::model::PageContext::default(),
        );
        let page = crate::durable::page::request(crate::database::Job { id: 7, target }, &scope)?;
        assert!(crate::durable::page::RetryPolicy::configured(&config, &page).is_err());
    }
    Ok(())
}
