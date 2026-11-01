use serde_json::{Value, json};

const DATABASE_COUNTERS: [&str; 7] = [
    "xact_commit",
    "xact_rollback",
    "blks_read",
    "blks_hit",
    "temp_files",
    "temp_bytes",
    "deadlocks",
];

fn difference(before: &Value, after: &Value, names: &[&str]) -> Value {
    let valid = before.get("stats_reset").is_some()
        && before.get("stats_reset") == after.get("stats_reset")
        && names.iter().all(|name| {
            before
                .get(*name)
                .and_then(Value::as_u64)
                .zip(after.get(*name).and_then(Value::as_u64))
                .is_some_and(|(first, last)| last >= first)
        });
    let counters = names
        .iter()
        .map(|name| {
            let delta = before
                .get(*name)
                .and_then(Value::as_u64)
                .zip(after.get(*name).and_then(Value::as_u64))
                .filter(|_| valid)
                .and_then(|(first, last)| last.checked_sub(first));
            ((*name).to_owned(), json!(delta))
        })
        .collect::<serde_json::Map<_, _>>();
    json!({"comparable":valid,"counters":counters})
}

pub fn counter_delta(before: &Value, after: &Value) -> Value {
    let empty = Value::Null;
    let first = before.get("database").map_or(&empty, |value| value);
    let last = after.get("database").map_or(&empty, |value| value);
    let same_database = first.get("datname").and_then(Value::as_str).is_some()
        && first.get("datname") == last.get("datname");
    let same_instance = before.get("postmaster_started_at").is_some()
        && before.get("postmaster_started_at") == after.get("postmaster_started_at");
    let database = difference(
        if same_database && same_instance {
            first
        } else {
            &empty
        },
        last,
        &DATABASE_COUNTERS,
    );
    let prior = if same_instance { before } else { &empty };
    let checkpoint = difference(
        prior.get("checkpointer").map_or(&empty, |value| value),
        after.get("checkpointer").map_or(&empty, |value| value),
        &["num_timed", "num_requested", "num_done", "buffers_written"],
    );
    let wal = difference(
        prior.get("wal").map_or(&empty, |value| value),
        after.get("wal").map_or(&empty, |value| value),
        &["wal_records", "wal_fpi", "wal_buffers_full"],
    );
    let elapsed_seconds = elapsed(before, after);
    json!({"same_database":same_database,"same_instance":same_instance,"database":database,"checkpointer":checkpoint,"wal":wal,
        "elapsed_seconds":elapsed_seconds,"io":super::interval::io_delta(prior,after),
        "io_timing_enabled":after.pointer("/settings/track_io_timing/value").and_then(Value::as_str)==Some("on"),
        "counter_units":{"blks_read":"8KiB PostgreSQL blocks; may be served by OS cache","temp_bytes":"temporary file bytes"}})
}

fn elapsed(before: &Value, after: &Value) -> Option<f64> {
    let parse = |value: &Value| {
        value
            .get("observed_at")
            .and_then(Value::as_str)
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
    };
    parse(before).zip(parse(after)).and_then(|(first, last)| {
        last.signed_duration_since(first)
            .to_std()
            .ok()
            .filter(|duration| !duration.is_zero())
            .map(|duration| duration.as_secs_f64())
    })
}
