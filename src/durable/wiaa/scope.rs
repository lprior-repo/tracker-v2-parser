use anyhow::{Context, Result, ensure};

use crate::{
    config::{Config, RunSettings},
    durable::{model, page::PageScope},
};

pub(super) fn bind(
    config: &Config,
    binding: &str,
    bounds: Option<(i64, i64)>,
    now_ms: i64,
) -> Result<PageScope> {
    let mut canonical = config.clone();
    canonical.validate()?;
    ensure!(
        model::page_config_digest(&canonical)? == model::page_config_digest(config)?,
        "WIAA configuration must be canonical"
    );
    let (deadline_ms, cooldown_ms) = if let Some(bounds) = bounds {
        bounds
    } else {
        let defaults = RunSettings::default();
        let run = config.run.map_or(defaults, std::convert::identity);
        (
            now_ms
                .checked_add(
                    i64::from(run.hours)
                        .checked_mul(3_600_000)
                        .context("WIAA duration overflow")?,
                )
                .context("WIAA deadline overflow")?,
            i64::from(run.cooldown_seconds)
                .checked_mul(1000)
                .context("WIAA cooldown overflow")?,
        )
    };
    ensure!(
        deadline_ms > 0
            && deadline_ms
                .checked_sub(now_ms)
                .context("WIAA deadline overflow")?
                <= 86_400_000,
        "WIAA deadline must be expired or within 24 hours"
    );
    ensure!(
        (15_000..=3_600_000).contains(&cooldown_ms),
        "WIAA cooldown must be 15 seconds to one hour"
    );
    Ok(PageScope {
        run_generation: model::digest(&(binding, "wiaa-directory"))?,
        config_sha256: model::page_config_digest(config)?,
        deadline_ms,
        cooldown_ms,
    })
}

#[cfg(test)]
mod tests;
