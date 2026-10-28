use anyhow::Result;
use track_scrapper_v2::{acquisition::AcquisitionPolicy, config::Config};

fn input(option: &str, athletic: bool, milesplit: bool) -> String {
    format!(
        "states=['WI']\ngraduation_years=[2027]\n[limits]\nmax_pages=7\n[sources.athleticnet]\nenabled={athletic}\n[sources.milesplit]\nenabled={milesplit}\n[acquisition]\nmode='public_results'\n{option}\n"
    )
}

#[test]
fn given_profile_coverage_setting_when_parsed_then_opt_in_is_explicit_and_defaults_remain_false()
-> Result<()> {
    for option in ["", "profile_coverage_only=false"] {
        let config = Config::parse(&input(option, false, true))?;
        assert!(matches!(
            config.acquisition,
            AcquisitionPolicy::PublicResults {
                profile_coverage_only: false,
                ..
            }
        ));
    }
    assert!(matches!(
        Config::parse("")?.acquisition,
        AcquisitionPolicy::Complete
    ));
    let config = Config::parse(&input("profile_coverage_only=true", false, true))?;
    assert!(matches!(
        config.acquisition,
        AcquisitionPolicy::PublicResults {
            profile_coverage_only: true,
            collect_contacts: false,
            ..
        }
    ));
    assert_eq!(config.states, vec!["WI"]);
    assert_eq!(config.graduation_years, vec![2027]);
    assert_eq!(config.limits.max_pages, 7);
    assert_eq!(config.limits.requests_per_second, 1.0);
    assert_eq!(config.limits.max_attempts, 3);
    Ok(())
}

#[test]
fn given_unsupported_source_or_typo_when_profile_coverage_is_configured_then_configuration_fails_clearly()
 {
    for (athletic, milesplit) in [(true, true), (true, false), (false, false)] {
        let error = Config::parse(&input("profile_coverage_only=true", athletic, milesplit))
            .expect_err("only MileSplit focused coverage supported");
        assert!(error.to_string().contains(
            "profile_coverage_only requires MileSplit enabled and Athletic.net disabled"
        ));
    }
    for option in [
        "profile_coverge_only=true",
        "profile_coverage_only='true'",
        "profile_coverage_only=1",
    ] {
        assert!(
            Config::parse(&input(option, false, true)).is_err(),
            "{option}"
        );
    }
    assert!(Config::parse(&input("profile_coverage_only=false", true, true)).is_ok());
}
