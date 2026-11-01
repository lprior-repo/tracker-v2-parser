use std::{collections::BTreeMap, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

#[tracing::instrument(skip_all)]
pub(super) async fn observe() -> Result<Value> {
    if !cfg!(target_os = "macos") {
        return Ok(json!({"platform":"unsupported","metrics":null}));
    }
    let kernel = output(
        "/usr/sbin/sysctl",
        &["-n", "hw.memsize", "kern.boottime", "vm.swapusage"],
    )
    .await?;
    let values = kernel.lines().take(4).collect::<Vec<_>>();
    ensure!(
        values.len() == 3,
        "host kernel report must contain three fields"
    );
    let physical = values
        .first()
        .context("missing physical memory")?
        .trim()
        .parse::<u64>()?;
    let boot = values.get(1).context("missing host boot identity")?;
    let swap = values.get(2).context("missing host swap allocation")?;
    let vm = output("/usr/bin/vm_stat", &[]).await?;
    Ok(
        json!({"platform":"macos","physical_memory_bytes":physical,"boot_time":boot,
        "vm":parse_vm(&vm)?,"swap_allocation":swap.trim(),
        "accounting":"VM counters are cumulative; swap allocation is not swap rate. Backend RSS must not be summed because shared pages overlap."}),
    )
}

fn parse_vm(text: &str) -> Result<Value> {
    let page_size = text
        .lines()
        .next()
        .and_then(|line| line.split("page size of ").nth(1))
        .and_then(|tail| tail.split_whitespace().next())
        .context("missing VM page size")?
        .parse::<u64>()?;
    ensure!(
        (4096..=65536).contains(&page_size),
        "unsupported VM page size"
    );
    let counters = text
        .lines()
        .skip(1)
        .take(100)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| {
            Ok((
                name.trim().trim_matches('"').to_owned(),
                value.trim().trim_end_matches('.').parse::<u64>()?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(json!({"page_size_bytes":page_size,"counters":counters}))
}

#[tracing::instrument(skip_all)]
async fn output(executable: &str, args: &[&str]) -> Result<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new(executable)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("host resource command timed out")??;
    ensure!(
        result.status.success() && result.stdout.len() <= 16384,
        "host resource command failed or exceeded16KiB"
    );
    Ok(String::from_utf8(result.stdout)?)
}
