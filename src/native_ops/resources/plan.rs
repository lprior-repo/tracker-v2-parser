use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{ConfigurationProfile, model::PlanArgs, telemetry::save};

pub fn configuration_plan(
    profile: ConfigurationProfile,
    host_gib: u16,
    stack_gib: u16,
) -> Result<Value> {
    ensure!(
        (8..=1024).contains(&host_gib),
        "host memory must be 8..1024 GiB"
    );
    ensure!(
        stack_gib > 0 && stack_gib <= 8 && stack_gib <= host_gib / 2,
        "stack budget must be at most 8 GiB and half host memory"
    );
    let changes = match profile {
        ConfigurationProfile::Observability => json!({
            "shared_preload_libraries_append":["pg_stat_statements"],
            "pg_stat_statements.track":"top",
            "pg_stat_statements.track_planning":"off",
            "log_checkpoints":"on","log_lock_waits":"on",
            "log_temp_files":"16MB","log_autovacuum_min_duration":"5s",
            "track_io_timing":"on","track_wal_io_timing":"on"
        }),
        ConfigurationProfile::Maintenance => json!({
            "autovacuum":"on","autovacuum_max_workers":"3",
            "autovacuum_work_mem":"64MB","maintenance_work_mem":"64MB",
            "table_options":{
                "results":{"autovacuum_analyze_scale_factor":"0.02","autovacuum_analyze_threshold":"1000"},
                "scrape_pages":{"autovacuum_analyze_scale_factor":"0.02","autovacuum_analyze_threshold":"1000"},
                "durable_page_operations":{"autovacuum_vacuum_scale_factor":"0.05","autovacuum_vacuum_threshold":"1000"},
                "durable_wire_requests":{"autovacuum_vacuum_scale_factor":"0.05","autovacuum_vacuum_threshold":"1000"}
            }
        }),
        ConfigurationProfile::CacheTrial => {
            ensure!(
                stack_gib >= 4,
                "cache trial requires at least 4 GiB stack budget"
            );
            json!({"shared_buffers":"512MB","work_mem":"4MB","max_connections":"40",
                "effective_io_concurrency":"16","io_method":"worker","io_workers":"3"})
        }
    };
    Ok(
        json!({"format":"native-postgres-plan-v1","profile":profile,"applied":false,
        "host_memory_gib":host_gib,"stack_memory_gib":stack_gib,
        "durability_required":{"fsync":"on","full_page_writes":"on","synchronous_commit":"on"},
        "changes":changes,
        "preconditions":["verified complete PostgreSQL and Restate recovery backup",
            "qualified same-invocation handoff and explicit frozen inputs",
            "disk guard on backup destination and live internal volume",
            "clock-overhead proof before enabling I/O timing",
            "baseline and post-change VM compression/swap deltas",
            "preserve existing shared_preload_libraries entries when appending module"],
        "restart_required":matches!(profile,ConfigurationProfile::Observability|ConfigurationProfile::CacheTrial),
        "postconditions":["verify pg_settings source/context/pending_restart",
            "verify actual query metrics and whole-stack physical footprint",
            "restore proof requires real isolated restore and replay; archive inventory alone is insufficient"],
        "rollback":"restore previous owned config during qualified handoff; never reset journals, counters or access stops"}),
    )
}

pub(super) fn run(args: &PlanArgs) -> Result<()> {
    let plan = configuration_plan(args.profile, args.host_memory_gib, args.stack_memory_gib)?;
    save(&args.output, &plan)
}
