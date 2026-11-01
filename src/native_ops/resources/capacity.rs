use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityBudget {
    pub database_bytes: u64,
    pub retained_copy_bytes: u64,
    pub scratch_bytes: u64,
    pub reserve_bytes: u64,
}

impl CapacityBudget {
    pub fn required_bytes(self) -> Result<u64> {
        self.database_bytes
            .checked_mul(2)
            .and_then(|value| value.checked_add(self.retained_copy_bytes.checked_mul(2)?))
            .and_then(|value| value.checked_add(self.scratch_bytes))
            .and_then(|value| value.checked_add(self.reserve_bytes))
            .context("resource capacity budget overflow")
    }

    pub fn ensure_available(self, available_bytes: u64) -> Result<()> {
        let required = self.required_bytes()?;
        ensure!(
            available_bytes >= required,
            "insufficient capacity: available {available_bytes} bytes, required {required} bytes; data retained"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityObservation {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub fn observe_capacity(path: &Path) -> Result<CapacityObservation> {
    ensure!(path.is_absolute(), "capacity path must be absolute");
    let canonical = path.canonicalize().context("capacity path must exist")?;
    let status = rustix::fs::statvfs(&canonical)?;
    Ok(CapacityObservation {
        total_bytes: status
            .f_blocks
            .checked_mul(status.f_frsize)
            .context("filesystem total size overflow")?,
        available_bytes: status
            .f_bavail
            .checked_mul(status.f_frsize)
            .context("filesystem available size overflow")?,
    })
}

#[derive(Debug, Serialize)]
pub struct BackupCapacityReceipt {
    pub destination: CapacityObservation,
    pub live_volume: CapacityObservation,
    pub budget: CapacityBudget,
    pub live_reserve_bytes: u64,
}

pub fn backup_preflight(
    database_bytes: u64,
    retained_copy_bytes: u64,
    destination: &Path,
    live: &Path,
) -> Result<BackupCapacityReceipt> {
    let destination = observe_capacity(destination)?;
    let live_volume = observe_capacity(live)?;
    let budget = CapacityBudget {
        database_bytes,
        retained_copy_bytes,
        scratch_bytes: 0,
        reserve_bytes: 2 * 1024 * 1024 * 1024,
    };
    budget.ensure_available(destination.available_bytes)?;
    ensure!(
        live_volume.available_bytes >= budget.reserve_bytes,
        "live PostgreSQL volume lacks 2 GiB reserve; existing data retained"
    );
    Ok(BackupCapacityReceipt {
        destination,
        live_volume,
        budget,
        live_reserve_bytes: budget.reserve_bytes,
    })
}
