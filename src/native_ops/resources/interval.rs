use serde_json::{Value, json};

pub(super) fn io_delta(before: &Value, after: &Value) -> Value {
    let first = before.get("io").and_then(Value::as_array);
    let last = after.get("io").and_then(Value::as_array);
    let values = last
        .into_iter()
        .flatten()
        .take(64)
        .map(|current| {
            let prior = first.into_iter().flatten().find(|row| {
                ["backend_type", "object", "context"]
                    .iter()
                    .all(|name| row.get(*name) == current.get(*name))
            });
            let empty = Value::Null;
            let prior = prior.map_or(&empty, |value| value);
            let valid = prior.get("stats_reset").is_some()
                && prior.get("stats_reset") == current.get("stats_reset");
            let counters = [
                "reads",
                "read_bytes",
                "writes",
                "write_bytes",
                "evictions",
                "fsyncs",
            ]
            .into_iter()
            .map(|name| {
                let delta = prior
                    .get(name)
                    .and_then(Value::as_u64)
                    .zip(current.get(name).and_then(Value::as_u64))
                    .filter(|_| valid)
                    .and_then(|(first, last)| last.checked_sub(first));
                (name.to_owned(), json!(delta))
            })
            .collect::<serde_json::Map<_, _>>();
            json!({"backend_type":current.get("backend_type"),"object":current.get("object"),
            "context":current.get("context"),"same_reset":valid,"counters":counters})
        })
        .collect::<Vec<_>>();
    json!(values)
}

pub(super) fn host_delta(before: &Value, after: &Value) -> Value {
    let page_size = after.pointer("/vm/page_size_bytes").and_then(Value::as_u64);
    let same_boot =
        after.get("boot_time").is_some() && before.get("boot_time") == after.get("boot_time");
    let same_page_size = same_boot
        && page_size.is_some()
        && page_size
            == before
                .pointer("/vm/page_size_bytes")
                .and_then(Value::as_u64);
    let pages = [
        "Swapins",
        "Swapouts",
        "Compressions",
        "Decompressions",
        "Pageins",
        "Pageouts",
    ]
    .into_iter()
    .map(|name| {
        let pointer = format!("/vm/counters/{name}");
        let delta = before
            .pointer(&pointer)
            .and_then(Value::as_u64)
            .zip(after.pointer(&pointer).and_then(Value::as_u64))
            .filter(|_| same_page_size)
            .and_then(|(first, last)| last.checked_sub(first));
        let bytes = delta
            .zip(page_size)
            .and_then(|(pages, size)| pages.checked_mul(size));
        (name.to_owned(), json!({"pages":delta,"bytes":bytes}))
    })
    .collect::<serde_json::Map<_, _>>();
    json!({"same_page_size":same_page_size,"counter_delta":pages,
        "interpretation":"Compression and decompression are page-volume changes, not net RAM growth; swap allocation is not swap rate."})
}
