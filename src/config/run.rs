use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunSettings {
    pub hours: u32,
    pub max_batches: u32,
    pub cooldown_seconds: u32,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            hours: 24,
            max_batches: 10_000,
            cooldown_seconds: 60,
        }
    }
}

impl RunSettings {
    pub(crate) fn validate(self) -> Result<()> {
        ensure!(
            (1..=24).contains(&self.hours),
            "run hours must be in 1..=24"
        );
        ensure!(
            (1..=10_000).contains(&self.max_batches),
            "run max_batches must be in 1..=10000"
        );
        ensure!(
            (15..=3_600).contains(&self.cooldown_seconds),
            "run cooldown_seconds must be in 15..=3600"
        );
        Ok(())
    }
}
