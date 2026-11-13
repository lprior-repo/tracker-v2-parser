use anyhow::{Context, Result, ensure};
use restate_sdk::context::InvocationHandle;

pub(in crate::durable::services) enum DispatchStep {
    Page(i64, InvocationHandle),
    Empty,
    PolicyAdvanced,
    DeadlineReached,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::durable::services) enum SelectionStop {
    #[default]
    Open,
    Empty,
    DeadlineReached,
}

pub(in crate::durable::services) fn advance_turn(used: usize, maximum: usize) -> Result<usize> {
    let next = used
        .checked_add(1)
        .context("selection turn counter overflow")?;
    ensure!(next <= maximum, "selection turn budget exceeded");
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::advance_turn;

    #[test]
    fn policy_and_page_turns_share_the_existing_bounded_budget() -> anyhow::Result<()> {
        assert_eq!(advance_turn(0, 2)?, 1);
        assert_eq!(advance_turn(1, 2)?, 2);
        assert!(advance_turn(2, 2).is_err());
        assert!(advance_turn(0, 0).is_err());
        assert!(advance_turn(usize::MAX, usize::MAX).is_err());
        Ok(())
    }
}
