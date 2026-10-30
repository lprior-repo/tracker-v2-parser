use anyhow::Result;
use track_scrapper_v2::config::Config;

#[test]
fn configured_run_profiles_keep_scope_and_future_cohorts_in_data() -> Result<()> {
    let wisconsin = Config::parse(include_str!("../config/runs/wisconsin.toml"))?;
    let mut others = Config::parse(include_str!("../config/runs/next-states.toml"))?;
    assert_eq!(wisconsin.states, ["WI"]);
    assert_eq!(others.states, ["CO", "IA", "IL", "MI", "MN"]);
    assert_eq!(others.graduation_years, [2027]);
    others.graduation_years = vec![2029, 2028, 2029];
    others.validate()?;
    assert_eq!(others.graduation_years, [2028, 2029]);
    assert_eq!(others.season, 2026);
    assert_eq!(others.limits.requests_per_second, 3.0);
    Ok(())
}

#[test]
fn configurable_public_results_reservation_is_bounded_and_defaults_are_digest_compatible()
-> Result<()> {
    let original = Config::parse("states=['WI']")?;
    let encoded = serde_json::to_value(&original)?;
    assert!(encoded["limits"].get("public_results_every").is_none());
    for interval in [1, 2, 8, 128] {
        let config = Config::parse(&format!(
            "states=['MI']\n[limits]\npublic_results_every={interval}"
        ))?;
        assert_eq!(
            config.limits.public_results_every.map(|value| value.get()),
            Some(interval)
        );
    }
    for value in ["0", "129", "-1", "1.5"] {
        assert!(Config::parse(&format!("[limits]\npublic_results_every={value}")).is_err());
    }
    Ok(())
}
