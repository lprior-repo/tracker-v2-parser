use std::{num::NonZeroUsize, path::Path};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{model::Source, scope::LOWER_48};

mod run;
pub use run::RunSettings;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportMode {
    #[default]
    Auto,
    Http,
    Browser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceSettings {
    pub enabled: bool,
    pub transport: TransportMode,
}

impl Default for SourceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: TransportMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub athleticnet: SourceSettings,
    pub milesplit: SourceSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub requests_per_second: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_results_every: Option<NonZeroUsize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_requests_per_second: Option<f64>,
    pub max_pages: usize,
    pub max_attempts: u32,
    pub concurrency: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            requests_per_second: 1.0,
            public_results_every: None,
            contact_requests_per_second: None,
            max_pages: 100,
            max_attempts: 3,
            concurrency: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub review_sheet: bool,
    pub rows_per_file: usize,
    pub per_athlete_sheets: bool,
    pub layout: crate::export::ExportLayout,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            name: None,
            review_sheet: false,
            rows_per_file: 100_000,
            per_athlete_sheets: false,
            layout: crate::export::ExportLayout::default(),
        }
    }
}

impl ExportSettings {
    pub(crate) fn validated_name(&self) -> Result<Option<&str>> {
        if let Some(name) = &self.name {
            ensure!(
                (1..=80).contains(&name.len())
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
                "export name must contain 1..80 ASCII letters, digits, hyphens or underscores"
            );
        }
        Ok(self.name.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunSettings>,
    pub acquisition: crate::acquisition::AcquisitionPolicy,
    pub states: Vec<String>,
    pub graduation_years: Vec<i32>,
    pub season: i32,
    pub limits: Limits,
    pub sources: Sources,
    pub export: ExportSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            run: None,
            acquisition: crate::acquisition::AcquisitionPolicy::default(),
            states: LOWER_48.iter().map(|state| (*state).to_owned()).collect(),
            graduation_years: vec![2027],
            season: 2026,
            limits: Limits::default(),
            sources: Sources::default(),
            export: ExportSettings::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let input = std::fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        Self::parse(&input)
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(input).context("parsing scraper configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&mut self) -> Result<()> {
        if let Some(run) = self.run {
            run.validate()?;
        }
        self.export.validated_name()?;
        self.acquisition.validate()?;
        self.validate_limits()?;
        ensure!(
            !self.export.review_sheet
                || self.export.layout == crate::export::ExportLayout::SingleSheet,
            "review_sheet requires single_sheet export layout"
        );
        ensure!(
            !self.acquisition.profile_coverage_only()
                || (!self.sources.athleticnet.enabled && self.sources.milesplit.enabled),
            "profile_coverage_only requires MileSplit enabled and Athletic.net disabled"
        );
        let mut states = self
            .states
            .iter()
            .map(|state| state.trim().to_ascii_uppercase())
            .collect::<Vec<_>>();
        ensure!(
            !states.is_empty()
                && states
                    .iter()
                    .all(|state| LOWER_48.contains(&state.as_str())),
            "states must be a nonempty subset of the lower 48"
        );
        ensure!(
            !self.graduation_years.is_empty()
                && self
                    .graduation_years
                    .iter()
                    .all(|year| (1900..=2200).contains(year)),
            "graduation_years must contain published cohort years in 1900..=2200"
        );
        ensure!(
            (1900..=2200).contains(&self.season),
            "season must be in 1900..=2200"
        );
        ensure!(
            self.sources.athleticnet.enabled || self.sources.milesplit.enabled,
            "at least one source must be enabled"
        );
        states.sort_unstable();
        states.dedup();
        self.states = states;
        self.graduation_years.sort_unstable();
        self.graduation_years.dedup();
        Ok(())
    }

    fn validate_limits(&self) -> Result<()> {
        ensure!(
            self.limits
                .public_results_every
                .is_none_or(|value| value.get() <= 128),
            "public_results_every must be in 1..=128"
        );
        let rate = self.limits.requests_per_second;
        ensure!(
            rate.is_finite() && rate > 0.0 && rate <= 3.0,
            "requests_per_second must be finite, positive and at most 3"
        );
        ensure!(
            self.limits
                .contact_requests_per_second
                .is_none_or(|rate| { rate.is_finite() && rate > 0.0 && rate <= 3.0 }),
            "contact_requests_per_second must be finite, positive and at most 3"
        );
        ensure!(
            (1..=100_000).contains(&self.limits.max_pages),
            "max_pages must be in 1..=100000"
        );
        ensure!(
            (1..=3).contains(&self.limits.max_attempts),
            "max_attempts must be in 1..=3"
        );
        ensure!(
            (1..=3).contains(&self.limits.concurrency),
            "concurrency must be in 1..=3"
        );
        ensure!(
            (1..=1_000_000).contains(&self.export.rows_per_file),
            "rows_per_file must be in 1..=1000000"
        );
        Ok(())
    }

    #[must_use]
    pub fn enabled_sources(&self) -> Vec<Source> {
        [Source::AthleticNet, Source::MileSplit]
            .into_iter()
            .filter(|source| self.settings(*source).enabled)
            .collect()
    }

    #[must_use]
    pub const fn settings(&self, source: Source) -> &SourceSettings {
        match source {
            Source::AthleticNet => &self.sources.athleticnet,
            Source::MileSplit => &self.sources.milesplit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_scope_is_canonical_and_cohort_is_never_inferred() -> Result<()> {
        let config = Config::parse("states=['wi',' MI ','wi']\ngraduation_years=[2028,2027,2028]")?;
        assert_eq!(config.states, vec!["MI", "WI"]);
        assert_eq!(config.graduation_years, vec![2027, 2028]);
        assert!(Config::parse("graduation_years=[]").is_err());
        assert!(Config::parse("states=['AK']").is_err());
        assert!(Config::parse("states=[]").is_err());
        Ok(())
    }

    #[test]
    fn misspelled_or_unbounded_configuration_is_rejected() {
        [
            "gradution_years=[2027]",
            "[limits]\nrequests_per_second=nan",
            "[limits]\nrequests_per_second=3.0001",
            "[limits]\nmax_attempts=4",
            "[limits]\nconcurrency=4",
            "[export]\nrows_per_file=0",
        ]
        .iter()
        .for_each(|input| assert!(Config::parse(input).is_err(), "{input}"));
    }

    #[test]
    fn given_explicit_bounded_rates_when_configured_then_defaults_and_single_concurrency_are_preserved()
    -> Result<()> {
        assert_eq!(Config::default().limits.requests_per_second, 1.0);
        for rate in [0.001, 1.0, 2.0, 3.0] {
            let config = Config::parse(&format!("[limits]\nrequests_per_second={rate}"))?;
            assert_eq!(config.limits.requests_per_second, rate);
            assert_eq!(config.limits.concurrency, 1);
        }
        for rate in ["nan", "inf", "-inf", "0.0", "-1.0", "3.0001"] {
            assert!(Config::parse(&format!("[limits]\nrequests_per_second={rate}")).is_err());
        }
        Ok(())
    }
}
