use super::model::DeadlineExtensionAuthorization;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidAuthority {
    Interval,
    HistoryBound,
    HistoryOrder,
}

impl std::fmt::Display for InvalidAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Interval => "deadline extension authorization must increase the prior deadline",
            Self::HistoryBound => "deadline authority exceeds 128 earlier deadlines",
            Self::HistoryOrder => "earlier deadlines must be positive, unique, ordered and before the previous deadline",
        })
    }
}

impl std::error::Error for InvalidAuthority {}

pub struct DeadlineAuthority<'a> {
    previous: i64,
    earlier: &'a [i64],
}

impl<'a> DeadlineAuthority<'a> {
    pub(crate) fn parse(
        authorization: &'a DeadlineExtensionAuthorization,
    ) -> Result<Self, InvalidAuthority> {
        if authorization.previous_deadline_ms <= 0
            || authorization.previous_deadline_ms >= authorization.effective_deadline_ms
        {
            return Err(InvalidAuthority::Interval);
        }
        if authorization.earlier_deadlines_ms.len() > 128 {
            return Err(InvalidAuthority::HistoryBound);
        }
        authorization
            .earlier_deadlines_ms
            .iter()
            .try_fold(0, |last, deadline| {
                if *deadline > last && *deadline < authorization.previous_deadline_ms {
                    Ok(*deadline)
                } else {
                    Err(InvalidAuthority::HistoryOrder)
                }
            })?;
        Ok(Self {
            previous: authorization.previous_deadline_ms,
            earlier: &authorization.earlier_deadlines_ms,
        })
    }

    pub(crate) fn permits(&self, owner_deadline: i64) -> bool {
        owner_deadline == self.previous || self.earlier.binary_search(&owner_deadline).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_accepts_only_exact_explicit_members() -> Result<(), InvalidAuthority> {
        let input = DeadlineExtensionAuthorization {
            previous_deadline_ms: 300,
            effective_deadline_ms: 400,
            earlier_deadlines_ms: vec![100, 200],
        };
        let parsed = DeadlineAuthority::parse(&input)?;
        assert!(parsed.permits(100) && parsed.permits(200) && parsed.permits(300));
        assert!(!parsed.permits(99) && !parsed.permits(150) && !parsed.permits(400));
        Ok(())
    }

    #[test]
    fn invalid_history_is_rejected_before_effects() {
        for earlier in [
            vec![0],
            vec![-1],
            vec![200, 100],
            vec![100, 100],
            vec![300],
            vec![400],
        ] {
            let input = DeadlineExtensionAuthorization {
                previous_deadline_ms: 300,
                effective_deadline_ms: 400,
                earlier_deadlines_ms: earlier,
            };
            assert_eq!(
                DeadlineAuthority::parse(&input).err(),
                Some(InvalidAuthority::HistoryOrder)
            );
        }
        let input = DeadlineExtensionAuthorization {
            previous_deadline_ms: 300,
            effective_deadline_ms: 400,
            earlier_deadlines_ms: (1..=129).collect(),
        };
        assert_eq!(
            DeadlineAuthority::parse(&input).err(),
            Some(InvalidAuthority::HistoryBound)
        );
    }
}
