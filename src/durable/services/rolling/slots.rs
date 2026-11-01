use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};

#[derive(Default)]
pub(super) struct ActivePages {
    pages: BTreeMap<usize, i64>,
}

impl ActivePages {
    pub(super) fn insert(&mut self, index: usize, page: i64, capacity: usize) -> Result<()> {
        ensure!((1..=3).contains(&capacity), "invalid rolling capacity");
        ensure!(self.pages.len() < capacity, "rolling capacity exceeded");
        ensure!(
            page > 0
                && !self.pages.contains_key(&index)
                && !self.pages.values().any(|id| *id == page),
            "invalid rolling page identity"
        );
        self.pages.insert(index, page);
        Ok(())
    }

    pub(super) fn remove(&mut self, index: usize) -> Result<i64> {
        self.pages
            .remove(&index)
            .context("rolling completion has no owned page")
    }

    pub(super) fn ids(&self) -> Vec<i64> {
        self.pages.values().copied().collect()
    }
}

pub(super) fn available(
    capacity: usize,
    active: usize,
    dispatched: usize,
    maximum: usize,
) -> Result<usize> {
    ensure!(
        (1..=3).contains(&capacity) && active <= capacity,
        "invalid rolling capacity"
    );
    ensure!(dispatched <= maximum, "rolling dispatch budget exceeded");
    Ok(capacity
        .saturating_sub(active)
        .min(maximum.saturating_sub(dispatched)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_completion_refills_only_free_slots_and_retains_all_active_page_ids() -> Result<()> {
        let mut active = ActivePages::default();
        for (index, page) in [(0, 7), (1, 8), (2, 9)] {
            active.insert(index, page, 3)?;
        }
        assert_eq!(available(3, 3, 3, 8)?, 0);
        assert_eq!(active.remove(1)?, 8);
        assert_eq!(active.ids(), vec![7, 9]);
        assert_eq!(available(3, 2, 3, 8)?, 1);
        active.insert(3, 10, 3)?;
        assert_eq!(active.ids(), vec![7, 9, 10]);
        assert!(active.insert(4, 11, 3).is_err());
        assert_eq!(active.remove(0)?, 7);
        assert!(active.insert(3, 12, 3).is_err());
        assert!(active.insert(4, 9, 3).is_err());
        assert!(active.insert(4, 0, 3).is_err());
        assert_eq!(active.ids(), vec![9, 10]);
        assert_eq!(available(3, 2, 8, 8)?, 0);
        assert!(available(4, 0, 0, 8).is_err());
        assert!(available(3, 4, 4, 8).is_err());
        assert!(available(3, 0, 9, 8).is_err());
        Ok(())
    }
}
