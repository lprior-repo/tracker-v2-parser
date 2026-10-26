use anyhow::{Result, bail};

pub const LOWER_48: [&str; 48] = [
    "AL", "AZ", "AR", "CA", "CO", "CT", "DE", "FL", "GA", "ID", "IL", "IN", "IA", "KS", "KY", "LA",
    "ME", "MD", "MA", "MI", "MN", "MS", "MO", "MT", "NE", "NV", "NH", "NJ", "NM", "NY", "NC", "ND",
    "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT", "VT", "VA", "WA", "WV", "WI", "WY",
];

pub fn states(value: &str) -> Result<Vec<String>> {
    if value.eq_ignore_ascii_case("lower48") || value.eq_ignore_ascii_case("all") {
        return Ok(LOWER_48.iter().map(|state| (*state).to_owned()).collect());
    }
    let mut selected = value
        .split(',')
        .map(|part| part.trim().to_ascii_uppercase())
        .collect::<Vec<_>>();
    if selected.is_empty()
        || selected
            .iter()
            .any(|state| !LOWER_48.contains(&state.as_str()))
    {
        bail!("--states must be lower48 or a comma-separated subset of the 48 contiguous states");
    }
    selected.sort_unstable();
    selected.dedup();
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower48_scope_has_no_unrequested_jurisdictions() -> Result<()> {
        let scope = states("lower48")?;
        assert_eq!(scope.len(), 48);
        assert!(
            !scope
                .iter()
                .any(|state| ["AK", "HI", "DC"].contains(&state.as_str()))
        );
        assert_eq!(states(" wi,MI,wi ")?, vec!["MI", "WI"]);
        assert!(states("AK").is_err());
        assert!(states("").is_err());
        Ok(())
    }
}
