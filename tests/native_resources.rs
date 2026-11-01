mod support;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::Connection;
use track_scrapper_v2::native_ops::resources::{
    CapacityBudget, ConfigurationProfile, configuration_plan, counter_delta, observe_capacity,
    snapshot_connection,
};

#[test]
fn given_backup_budget_when_headroom_is_one_byte_short_then_backup_is_denied_without_overflow()
-> Result<()> {
    let budget = CapacityBudget {
        database_bytes: 100,
        retained_copy_bytes: 50,
        scratch_bytes: 25,
        reserve_bytes: 75,
    };
    assert_eq!(budget.required_bytes()?, 400);
    assert!(budget.ensure_available(399).is_err());
    assert!(budget.ensure_available(400).is_ok());
    assert!(
        CapacityBudget {
            database_bytes: u64::MAX,
            ..budget
        }
        .required_bytes()
        .is_err()
    );
    Ok(())
}

#[test]
fn given_existing_destination_when_capacity_is_observed_then_available_is_bounded_by_total()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let capacity = observe_capacity(root.path())?;
    assert!(capacity.total_bytes > 0);
    assert!(capacity.available_bytes <= capacity.total_bytes);
    assert!(observe_capacity(std::path::Path::new("relative")).is_err());
    assert!(observe_capacity(&root.path().join("missing")).is_err());
    Ok(())
}

#[test]
fn given_statistics_reset_or_counter_regression_when_compared_then_all_database_deltas_are_unavailable()
 {
    let before = sample(10, "reset-a");
    let mut after = sample(20, "reset-a");
    assert_eq!(
        counter_delta(&before, &after)["database"]["counters"]["temp_bytes"],
        10
    );
    after["database"]["stats_reset"] = json!("reset-b");
    assert_eq!(
        counter_delta(&before, &after)["database"]["comparable"],
        false
    );
    after = sample(9, "reset-a");
    assert_eq!(
        counter_delta(&before, &after)["database"]["counters"]["temp_bytes"],
        Value::Null
    );
    assert_eq!(
        counter_delta(&Value::Null, &Value::Null)["same_database"],
        false
    );
}

#[test]
fn given_different_database_when_counters_increase_then_delta_does_not_cross_database_identity() {
    let before = sample(10, "reset-a");
    let mut after = sample(20, "reset-a");
    after["database"]["datname"] = json!("different");
    let delta = counter_delta(&before, &after);
    assert_eq!(delta["same_database"], false);
    assert_eq!(delta["database"]["counters"]["blks_read"], Value::Null);
    assert_eq!(delta["io_timing_enabled"], false);
}

fn sample(number: u64, reset: &str) -> Value {
    json!({"postmaster_started_at":"2026-11-01T12:00:00Z","observed_at":"2026-11-01T13:00:00Z","database":{"datname":"test","stats_reset":reset,
        "xact_commit":number,"xact_rollback":number,"blks_read":number,"blks_hit":number,
        "temp_files":number,"temp_bytes":number,"deadlocks":number}})
}

#[test]
fn given_shared_mac_budget_when_configuration_profiles_are_built_then_durability_and_memory_caps_survive()
-> Result<()> {
    for profile in [
        ConfigurationProfile::Observability,
        ConfigurationProfile::Maintenance,
        ConfigurationProfile::CacheTrial,
    ] {
        let plan = configuration_plan(profile, 16, 8)?;
        assert_eq!(plan["applied"], false);
        assert_eq!(
            plan["durability_required"],
            json!({"fsync":"on","full_page_writes":"on","synchronous_commit":"on"})
        );
        assert_eq!(plan["stack_memory_gib"], 8);
        assert!(configuration_plan(profile, 16, 9).is_err());
        assert!(configuration_plan(profile, 8, 8).is_err());
    }
    let cache = configuration_plan(ConfigurationProfile::CacheTrial, 16, 8)?;
    assert_eq!(cache["changes"]["work_mem"], "4MB");
    assert_eq!(cache["changes"]["shared_buffers"], "512MB");
    assert!(configuration_plan(ConfigurationProfile::CacheTrial, 16, 2).is_err());
    Ok(())
}

#[tokio::test]
async fn given_real_database_when_resource_snapshot_runs_then_business_facts_and_schema_are_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let fixture_database=track_scrapper_v2::database::Database::connect(&test.url).await?;
            sqlx::query("INSERT INTO athletes(name,gender,graduation_year,state) VALUES('Resource Fixture','M',2027,'MI')")
                .execute(fixture_database.pool()).await?;
            let options = test.url.parse::<url::Url>()?;
            let database = options.path().trim_start_matches('/');
            let before: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(&test.pool)
                .await?;
            let mut connection = sqlx::PgConnection::connect(&test.url).await?;
            let observation = snapshot_connection(&mut connection).await?;
            connection.close().await?;
            assert_eq!(observation["transaction_read_only"], "on");
            assert_eq!(observation["postgres_major"], 18);
            assert_eq!(observation["database"]["datname"], database);
            let after: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(before,1);
            assert_eq!(before, after);
            fixture_database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_resource_plan_cli_when_repeated_then_private_report_is_never_overwritten()
-> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let output = root.path().join("plan.json");
    let arguments = [
        "resources",
        "plan",
        "--output",
        output.to_str().unwrap(),
        "--profile",
        "observability",
    ];
    let first = tokio::process::Command::new(env!("CARGO_BIN_EXE_native-ops"))
        .args(arguments)
        .output()
        .await?;
    support::assert_success(&first);
    let bytes = std::fs::read(&output)?;
    let plan: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        plan["changes"]["shared_preload_libraries_append"],
        json!(["pg_stat_statements"])
    );
    assert_eq!(
        std::fs::metadata(&output)?.permissions().mode() & 0o777,
        0o600
    );
    let second = tokio::process::Command::new(env!("CARGO_BIN_EXE_native-ops"))
        .args(arguments)
        .output()
        .await?;
    assert!(!second.status.success());
    assert_eq!(std::fs::read(&output)?, bytes);
    Ok(())
}

#[tokio::test]
async fn given_unbounded_sampling_when_cli_parses_then_no_report_is_created() -> Result<()> {
    let root = tempfile::tempdir()?;
    let output = root.path().join("report.json");
    let result = tokio::process::Command::new(env!("CARGO_BIN_EXE_native-ops"))
        .args([
            "resources",
            "snapshot",
            "--database",
            "fixture",
            "--output",
            output.to_str().unwrap(),
            "--capacity-path",
            root.path().to_str().unwrap(),
            "--samples",
            "61",
        ])
        .output()
        .await?;
    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(String::from_utf8(result.stderr)?.contains("invalid value"));
    Ok(())
}

#[tokio::test]
async fn given_native_resource_cli_when_sampling_twice_then_private_report_contains_readonly_metrics_and_deltas()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let options = test.url.parse::<sqlx::postgres::PgConnectOptions>()?;
            let parsed = test.url.parse::<url::Url>()?;
            let output = test.directory().join("resources.json");
            let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_native-ops"));
            command
                .args([
                    "resources",
                    "snapshot",
                    "--database",
                    parsed.path().trim_start_matches('/'),
                    "--output",
                    output.to_str().unwrap(),
                    "--capacity-path",
                    test.directory().to_str().unwrap(),
                    "--samples",
                    "2",
                    "--interval-seconds",
                    "1",
                ])
                .env("PGHOST", options.get_host())
                .env("PGPORT", options.get_port().to_string())
                .env("PGUSER", options.get_username())
                .env_remove("PGSERVICE")
                .env_remove("PGHOSTADDR")
                .kill_on_drop(true);
            if let Some(password) = parsed.password() {
                let form = format!("value={}", password.replace('+', "%2B"));
                let decoded = url::form_urlencoded::parse(form.as_bytes())
                    .next()
                    .unwrap()
                    .1
                    .into_owned();
                command.env("PGPASSWORD", decoded);
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(45), command.output())
                .await??;
            support::assert_success(&result);
            let report: Value = serde_json::from_slice(&std::fs::read(&output)?)?;
            assert_eq!(report["read_only"], true);
            assert_eq!(report["samples"].as_array().unwrap().len(), 2);
            assert_eq!(
                report["samples"][0]["postgres"]["transaction_read_only"],
                "on"
            );
            assert_eq!(report["deltas"].as_array().unwrap().len(), 1);
            assert_eq!(report["deltas"][0]["postgres"]["same_instance"], true);
            assert!(!String::from_utf8(std::fs::read(&output)?)?.contains("SELECT"));
            Ok(())
        })
    })
    .await
}

#[test]
fn given_existing_preload_when_observability_configuration_is_prepared_then_modules_are_preserved_and_artifact_is_unapplied()
-> Result<()> {
    use track_scrapper_v2::native_ops::resources::prepare_configuration;
    let evidence = json!({"postgres_major":18,"transaction_read_only":"on","observed_at":"2026-11-01T14:00:00Z",
        "settings":{"fsync":{"value":"on"},"full_page_writes":{"value":"on"},"synchronous_commit":{"value":"on"},
        "shared_preload_libraries":{"value":"auto_explain,pg_prewarm"}}});
    let prepared = prepare_configuration(ConfigurationProfile::Observability, 16, 8, &evidence)?;
    assert_eq!(prepared["applied"], false);
    let configuration = prepared["configuration"].as_str().unwrap();
    assert!(
        configuration
            .contains("shared_preload_libraries = 'auto_explain,pg_prewarm,pg_stat_statements'")
    );
    assert!(configuration.contains("fsync = 'on'"));
    assert_eq!(prepared["configuration_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        prepare_configuration(ConfigurationProfile::Observability, 16, 8, &evidence)?,
        prepared
    );
    Ok(())
}

#[test]
fn given_weakened_durability_or_untrusted_preload_when_configuration_is_prepared_then_no_artifact_is_approved()
 {
    use track_scrapper_v2::native_ops::resources::prepare_configuration;
    let mut evidence = json!({"postgres_major":18,"transaction_read_only":"on",
        "settings":{"fsync":{"value":"on"},"full_page_writes":{"value":"on"},"synchronous_commit":{"value":"off"},
        "shared_preload_libraries":{"value":"pg_stat_statements"}}});
    assert!(prepare_configuration(ConfigurationProfile::Observability, 16, 8, &evidence).is_err());
    evidence["settings"]["synchronous_commit"]["value"] = json!("on");
    evidence["settings"]["shared_preload_libraries"]["value"] = json!("module'\nfsync=off");
    assert!(prepare_configuration(ConfigurationProfile::Observability, 16, 8, &evidence).is_err());
    evidence["postgres_major"] = json!(17);
    assert!(prepare_configuration(ConfigurationProfile::Maintenance, 16, 8, &evidence).is_err());
}

#[test]
fn given_stale_or_future_observation_when_configuration_age_is_validated_then_only_bounded_fresh_evidence_is_accepted()
-> Result<()> {
    use track_scrapper_v2::native_ops::resources::validate_observation_age;
    let now = "2026-11-01T14:00:00Z".parse::<chrono::DateTime<chrono::Utc>>()?;
    assert!(validate_observation_age(&json!({"observed_at":"2026-11-01T14:00:00Z"}), now).is_ok());
    assert!(validate_observation_age(&json!({"observed_at":"2026-11-01T13:58:00Z"}), now).is_ok());
    assert!(validate_observation_age(&json!({"observed_at":"2026-11-01T13:57:59Z"}), now).is_err());
    assert!(validate_observation_age(&json!({"observed_at":"2026-11-01T14:00:01Z"}), now).is_err());
    Ok(())
}

#[test]
fn given_fractional_future_observation_when_configuration_is_prepared_then_negative_age_is_not_truncated_to_zero()
-> Result<()> {
    use track_scrapper_v2::native_ops::resources::validate_observation_age;
    let now = "2026-11-01T14:00:00Z".parse::<chrono::DateTime<chrono::Utc>>()?;
    assert!(
        validate_observation_age(&json!({"observed_at":"2026-11-01T14:00:00.001Z"}), now).is_err()
    );
    Ok(())
}
