use anyhow::{Result, ensure};

const PHASES: [&str; 4] = ["-schools", "-contacts", "-results", "-complete"];

pub fn validate_config(config: &crate::config::Config, inherited: &[String]) -> Result<()> {
    ensure!(
        inherited.is_empty()
            || !config
                .states
                .iter()
                .any(|state| state.eq_ignore_ascii_case("WI")),
        "inherited retry campaigns are unsupported for Wisconsin because legacy WIAA scopes require explicit reconciliation"
    );
    Ok(())
}

pub fn campaign(generation: &str) -> &str {
    PHASES
        .into_iter()
        .find_map(|suffix| generation.strip_suffix(suffix))
        .map_or(generation, |value| value)
}

pub fn validate(current: &str, inherited: &[String]) -> Result<()> {
    ensure!(
        inherited.len() <= 8,
        "retry lineage exceeds eight campaigns"
    );
    ensure!(
        inherited.is_empty() || campaign(current) == current,
        "inherited retry campaign cannot have a phase-suffixed current root"
    );
    inherited.iter().enumerate().try_for_each(|(index, value)| {
        super::model::identifier(value)?;
        ensure!(value.len() <= 100, "retry ancestor exceeds 100 bytes");
        ensure!(
            campaign(value) == value,
            "retry ancestor must be a campaign without a phase suffix"
        );
        ensure!(
            value != current,
            "retry lineage cannot inherit its current campaign"
        );
        ensure!(
            !inherited.iter().take(index).any(|earlier| earlier == value),
            "retry lineage contains a duplicate campaign"
        );
        Ok(())
    })
}

pub fn families<'a>(campaigns: impl Iterator<Item = &'a str>) -> Vec<String> {
    campaigns
        .flat_map(|value| {
            std::iter::once(value.to_owned()).chain(
                PHASES
                    .into_iter()
                    .map(move |suffix| format!("{value}{suffix}")),
            )
        })
        .collect()
}
