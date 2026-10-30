use anyhow::{Result, bail};
use clap::Parser;

use super::{DurableArgs, DurableCommand, RunOverrides, commands};
use crate::config::Config;

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    durable: DurableArgs,
}

fn overrides(extra: &[&str]) -> Result<RunOverrides> {
    let parsed = Arguments::try_parse_from(
        ["test", "crawl", "--generation", "configured-run"]
            .into_iter()
            .chain(extra.iter().copied()),
    )?;
    let DurableCommand::Crawl { run, .. } = parsed.durable.command else {
        bail!("expected crawl command")
    };
    Ok(run)
}

#[test]
fn given_run_settings_when_crawl_is_built_then_request_uses_configured_bounds() -> Result<()> {
    let config =
        Config::parse("states=['WI']\n[run]\nhours=6\nmax_batches=25\ncooldown_seconds=120")?;
    let request = commands::crawl_request(config, "configured-run".into(), overrides(&[])?, 1000)?;
    assert_eq!(request.deadline_ms, 21_601_000);
    assert_eq!(request.max_batches, 25);
    assert_eq!(request.cooldown_ms, 120_000);
    assert_eq!(request.workflow_key(), "configured-run-000000");
    assert_eq!(request.page_scope()?.cooldown_ms, 120_000);
    let encoded = serde_json::to_value(&request)?;
    assert_eq!(encoded["config"]["run"]["hours"], 6);
    assert_eq!(encoded["deadline_ms"], 21_601_000);
    Ok(())
}

#[test]
fn given_cli_overrides_when_crawl_is_built_then_flags_take_precedence() -> Result<()> {
    let config = Config::parse("[run]\nhours=6\nmax_batches=25\ncooldown_seconds=120")?;
    let flags = overrides(&[
        "--hours",
        "2",
        "--max-batches",
        "7",
        "--cooldown-seconds",
        "15",
    ])?;
    let request = commands::crawl_request(config, "overridden-run".into(), flags, 1000)?;
    assert_eq!(request.deadline_ms, 7_201_000);
    assert_eq!(request.max_batches, 7);
    assert_eq!(request.cooldown_ms, 15_000);
    let effective = request
        .config
        .run
        .ok_or_else(|| anyhow::anyhow!("resolved run settings missing"))?;
    assert_eq!(
        (
            effective.hours,
            effective.max_batches,
            effective.cooldown_seconds
        ),
        (2, 7, 15)
    );
    let partial = commands::crawl_request(
        Config::parse("[run]\nhours=6\nmax_batches=25\ncooldown_seconds=120")?,
        "partial-run".into(),
        overrides(&["--hours", "1"])?,
        1000,
    )?;
    assert_eq!((partial.max_batches, partial.cooldown_ms), (25, 120_000));
    Ok(())
}

#[test]
fn given_legacy_config_when_no_flags_are_set_then_original_request_defaults_remain() -> Result<()> {
    let config = Config::parse("states=['WI']")?;
    let baseline = concat!(
        r#"{"acquisition":{"mode":"complete"},"states":["WI"],"graduation_years":[2027],"season":2026,"#,
        r#""limits":{"requests_per_second":1.0,"max_pages":100,"max_attempts":3,"concurrency":1},"#,
        r#""sources":{"athleticnet":{"enabled":true,"transport":"auto"},"milesplit":{"enabled":true,"transport":"auto"}},"#,
        r#""export":{"rows_per_file":100000,"per_athlete_sheets":false,"layout":"detailed"}}"#,
    );
    assert_eq!(serde_json::to_string(&config)?, baseline);
    assert_eq!(
        crate::durable::model::page_config_digest(&config)?,
        "ad2511ee99d77d866b9647ecb8dfd75225571788e15f5ba98cc86cabf4aeb3b2"
    );
    assert!(serde_json::to_value(&config)?.get("run").is_none());
    let request = commands::crawl_request(config, "legacy-run".into(), overrides(&[])?, 1000)?;
    assert_eq!(request.deadline_ms, 86_401_000);
    assert_eq!(request.max_batches, 10_000);
    assert_eq!(request.cooldown_ms, 60_000);
    let encoded = serde_json::to_string(&request)?;
    let decoded: crate::durable::model::CrawlRequest = serde_json::from_str(&encoded)?;
    assert_eq!(serde_json::to_string(&decoded)?, encoded);
    assert_eq!(
        decoded.config.run,
        Some(crate::config::RunSettings::default())
    );
    let mut legacy = serde_json::to_value(&request)?;
    legacy["config"]
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config object"))?
        .remove("run");
    let legacy: crate::durable::model::CrawlRequest = serde_json::from_value(legacy)?;
    assert!(legacy.config.run.is_none());
    assert_eq!(
        legacy.page_scope()?.config_sha256,
        "ad2511ee99d77d866b9647ecb8dfd75225571788e15f5ba98cc86cabf4aeb3b2"
    );
    let encoded = serde_json::to_string(&legacy)?;
    assert_eq!(
        serde_json::to_string(
            &serde_json::from_str::<crate::durable::model::CrawlRequest>(&encoded)?
        )?,
        encoded
    );
    Ok(())
}

#[test]
fn given_run_limits_when_values_are_invalid_then_config_and_cli_reject_them() {
    for (field, flag, values) in [
        ("hours", "--hours", ["0", "25", "-1", "1.5", "bad"]),
        (
            "max_batches",
            "--max-batches",
            ["0", "10001", "-1", "1.5", "bad"],
        ),
        (
            "cooldown_seconds",
            "--cooldown-seconds",
            ["14", "3601", "-1", "1.5", "bad"],
        ),
    ] {
        for value in values {
            assert!(Config::parse(&format!("[run]\n{field}={value}")).is_err());
            assert!(overrides(&[flag, value]).is_err());
        }
    }
    assert!(Config::parse("[run]\nhuors=1").is_err());
}

#[test]
fn given_partial_or_boundary_settings_when_loaded_then_valid_bounds_and_defaults_resolve()
-> Result<()> {
    for settings in [
        "hours=1\nmax_batches=1\ncooldown_seconds=15",
        "hours=24\nmax_batches=10000\ncooldown_seconds=3600",
        "hours=12",
        "",
    ] {
        let config = Config::parse(&format!("[run]\n{settings}"))?;
        commands::crawl_request(config, "boundary-run".into(), overrides(&[])?, 1000)?;
    }
    let request = commands::crawl_request(
        Config::parse("[run]\nhours=12")?,
        "partial-run".into(),
        overrides(&[])?,
        1000,
    )?;
    assert_eq!(request.deadline_ms, 43_201_000);
    assert_eq!((request.max_batches, request.cooldown_ms), (10_000, 60_000));
    Ok(())
}

#[test]
fn given_run_templates_when_loaded_then_operational_settings_are_explicit() -> Result<()> {
    for input in [
        include_str!("../../../config/runs/wisconsin.toml"),
        include_str!("../../../config/runs/next-states.toml"),
    ] {
        let config = Config::parse(input)?;
        if config.states == ["WI"] {
            assert_eq!(
                serde_json::to_value(&config.acquisition)?["reviewed_legacy_failures"],
                serde_json::json!(["wiaa_script_sanitization"])
            );
        }
        let settings = config
            .run
            .ok_or_else(|| anyhow::anyhow!("run settings missing"))?;
        assert_eq!(
            (
                settings.hours,
                settings.max_batches,
                settings.cooldown_seconds
            ),
            (24, 10_000, 60)
        );
        let request =
            commands::crawl_request(config, "template-run".into(), overrides(&[])?, 1000)?;
        assert_eq!(request.max_batches, settings.max_batches);
    }
    Ok(())
}
