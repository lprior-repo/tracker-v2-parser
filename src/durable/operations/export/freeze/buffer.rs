use anyhow::{Context, Result};

use super::{Batch, FrozenPlan, Workspace, append};
use crate::database::{EXPORT_BATCH_BYTES, EXPORT_BATCH_ROWS};

pub(super) struct Buffer {
    plan: FrozenPlan,
    pending: Option<Batch>,
    bytes: usize,
}

impl Buffer {
    pub(super) const fn new(plan: FrozenPlan) -> Self {
        Self {
            plan,
            pending: None,
            bytes: 0,
        }
    }

    fn fits(&self, bytes: usize, rows: usize) -> bool {
        self.bytes
            .checked_add(bytes)
            .is_some_and(|total| total <= EXPORT_BATCH_BYTES)
            && self
                .pending
                .as_ref()
                .map_or(0, |batch| batch.records.len())
                .checked_add(rows)
                .is_some_and(|total| total <= EXPORT_BATCH_ROWS)
    }

    async fn flush(mut self, workspace: &Workspace) -> Result<Self> {
        if let Some(batch) = self.pending.take() {
            self.plan = store(workspace.clone(), self.plan, batch).await?;
            self.bytes = 0;
        }
        Ok(self)
    }

    pub(super) async fn push(mut self, workspace: &Workspace, batch: Batch) -> Result<Self> {
        let records =
            !batch.records.is_empty() && batch.issues.is_empty() && batch.review.is_empty();
        let bytes = serde_json::to_vec(&batch)?.len();
        if !records || !self.fits(bytes, batch.records.len()) {
            self = self.flush(workspace).await?;
        }
        if !records || bytes > EXPORT_BATCH_BYTES || batch.records.len() > EXPORT_BATCH_ROWS {
            self.plan = store(workspace.clone(), self.plan, batch).await?;
            return Ok(self);
        }
        if let Some(pending) = &mut self.pending {
            pending.records.try_reserve(batch.records.len())?;
            pending.records.extend(batch.records);
        } else {
            self.pending = Some(batch);
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("projected batch byte overflow")?;
        Ok(self)
    }

    pub(super) async fn finish(self, workspace: &Workspace) -> Result<FrozenPlan> {
        Ok(self.flush(workspace).await?.plan)
    }
}

async fn store(workspace: Workspace, plan: FrozenPlan, batch: Batch) -> Result<FrozenPlan> {
    tokio::task::spawn_blocking(move || append(&workspace, plan, &batch))
        .await
        .context("joining bounded frozen chunk writer")?
}
