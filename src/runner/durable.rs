use anyhow::Result;

use crate::{config::Config, database::Database};

pub(crate) async fn check_stops(database: &Database, config: &Config) -> Result<()> {
    super::check_source_stops(database, config).await
}

pub async fn bootstrap(database: &Database, config: &Config) -> Result<()> {
    check_stops(database, config).await?;
    if !config.acquisition.profile_coverage_only() {
        database
            .enqueue(&super::Runner::discover_targets(config)?, false)
            .await?;
    }
    if config.acquisition.includes_contacts() && !config.acquisition.profile_coverage_only() {
        database
            .backfill_contacts(&config.states, &config.enabled_sources(), config.season)
            .await?;
    }
    Ok(())
}
