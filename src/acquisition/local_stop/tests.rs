use super::*;
use serde_json::json;

fn evidence() -> LocalContactStop {
    let address = "https://co.milesplit.com/teams/15064-school/admin";
    LocalContactStop {
        address:address.into(), cursor:json!({"url":address,"source":"milesplit","kind":"contacts","context":{},"access_stop_url":"https://co.milesplit.com"}),
        attempts:1,detail:Some("browser interception failed: source access stopped at https://co.milesplit.com (HTTP None); human review required".into()),
        consistent_attempt:true,operations:1,attempts_recorded:1,wires:1,redirects:1,captures:false,origin_denied:false,
    }
}

#[test]
fn given_exact_local_admin_redirect_when_classified_then_only_public_provider_routes_qualify() {
    assert!(evidence().qualifies("https://co.milesplit.com"));
    assert_eq!(
        public_origin("https://co.milesplit.com/athletes/14603706-runner/stats"),
        Some("https://co.milesplit.com".into())
    );
    for address in [
        "https://co.milesplit.com/teams/15064-school/admin",
        "https://co.milesplit.com/login",
        "https://co.milesplit.com/teams/15064-school/admin/",
        "https://co.milesplit.com/teams/15064-school/admin/settings",
        "https://co.milesplit.com/athletes/1/control",
        "https://co.milesplit.com/athletes/%31/stats",
        "http://co.milesplit.com/athletes/1",
        "https://user@co.milesplit.com/athletes/1",
        "https://evil.milesplit.com/athletes/1",
        "https://co.milesplit.com/athletes/1?token=x",
        "https://co.milesplit.com/athletes/0/stats",
    ] {
        assert!(public_origin(address).is_none(), "{address}");
    }
}

#[test]
fn given_missing_or_conflicting_local_stop_evidence_when_classified_then_it_fails_closed() {
    let mutations: Vec<Box<dyn Fn(&mut LocalContactStop)>> = vec![
        Box::new(|e| e.attempts = 2),
        Box::new(|e| e.consistent_attempt = false),
        Box::new(|e| e.operations = 2),
        Box::new(|e| e.attempts_recorded = 2),
        Box::new(|e| e.wires = 0),
        Box::new(|e| e.wires = 2),
        Box::new(|e| e.redirects = 0),
        Box::new(|e| e.captures = true),
        Box::new(|e| e.origin_denied = true),
        Box::new(|e| e.detail = None),
        Box::new(|e| e.detail = Some("HTTP403 remote denial".into())),
        Box::new(|e| e.cursor["access_stop_url"] = json!("https://www.milesplit.com")),
        Box::new(|e| e.cursor["kind"] = json!("athlete")),
        Box::new(|e| e.cursor["source"] = json!("athleticnet")),
        Box::new(|e| e.cursor["body"] = json!({"query":"private"})),
        Box::new(|e| e.cursor["method"] = json!("POST")),
        Box::new(|e| e.cursor["token_url"] = json!("https://co.milesplit.com/api/private")),
    ];
    for change in mutations {
        let mut e = evidence();
        change(&mut e);
        assert!(!e.qualifies("https://co.milesplit.com"));
    }
    assert!(!evidence().qualifies("https://wi.milesplit.com"));
}

#[test]
fn given_public_results_policy_when_team_admin_is_discovered_then_it_is_excluded_before_acquisition()
-> anyhow::Result<()> {
    let config: crate::config::Config = toml::from_str(
        "states=['CO']\n[acquisition]\nmode='public_results'\ncollect_contacts=true",
    )?;
    let target: Target = serde_json::from_value(evidence().cursor)?;
    assert!(config.acquisition.exclusion_reason(&target).is_some());
    Ok(())
}

#[test]
fn given_admin_descendants_when_discovered_then_public_scope_excludes_every_variant()
-> anyhow::Result<()> {
    let config = crate::config::Config::parse(
        "states=['CO']\n[acquisition]\nmode='public_results'\ncollect_contacts=true",
    )?;
    for address in [
        "https://co.milesplit.com/teams/15064-school/admin/",
        "https://co.milesplit.com/teams/15064-school/admin/settings",
        "https://co.milesplit.com/teams/15064-school/ADMIN",
    ] {
        let mut target: Target = serde_json::from_value(evidence().cursor)?;
        target.url = address.into();
        assert!(admin_route(address));
        assert!(config.acquisition.exclusion_reason(&target).is_some());
        assert!(public_origin(address).is_none());
    }
    for address in [
        "https://co.milesplit.com/teams",
        "https://co.milesplit.com/results?season=outdoor&year=2026&level=hs&page=1",
        "https://co.milesplit.com/meets/123-meet/results/456/raw",
        "https://co.milesplit.com/athletes/123/stats?page=2",
        "https://co.milesplit.com/athletes/123-runner/articles",
        "https://co.milesplit.com/athletes/123-runner/articles?page=2",
    ] {
        assert!(public_origin(address).is_some(), "{address}");
    }
    for address in [
        "https://co.milesplit.com/results?season=outdoor&year=2026&level=hs&page=1&token=private",
        "https://co.milesplit.com/results?page=1&page=2",
        "https://co.milesplit.com/athletes/1/edit",
        "https://co.milesplit.com/athletes/123-runner/articles/edit",
        "https://co.milesplit.com/athletes/123-runner/articles?token=private",
    ] {
        assert!(public_origin(address).is_none(), "{address}");
    }
    Ok(())
}
