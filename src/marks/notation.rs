#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Time,
    Distance,
    Points,
}

impl Measure {
    #[must_use]
    pub const fn better(self, candidate: i64, incumbent: i64) -> bool {
        match self {
            Self::Time => candidate < incumbent,
            Self::Distance | Self::Points => candidate > incumbent,
        }
    }

    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Time => "ns",
            Self::Distance => "um",
            Self::Points => "centipoints",
        }
    }
}

#[must_use]
pub fn parse_mark(raw: &str, measure: Measure) -> Option<i64> {
    let raw = raw.trim();
    let value = match measure {
        Measure::Time => time(time_body(raw)),
        Measure::Distance => distance(raw),
        Measure::Points => points(raw),
    }?;
    (value > 0).then_some(value)
}

fn integer(raw: &str) -> Option<i128> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

fn scaled(raw: &str, scale: i64) -> Option<i64> {
    let raw = raw.trim();
    match raw.split_once('.') {
        None => scale_decimal(integer(raw)?, None, scale),
        Some((whole, fraction)) => scale_decimal(integer(whole)?, Some(fraction), scale),
    }
}

fn scale_decimal(whole: i128, fraction: Option<&str>, scale: i64) -> Option<i64> {
    let scale = i128::from(scale);
    let whole = whole.checked_mul(scale)?;
    let value = match fraction {
        None => whole,
        Some(fraction) => {
            let digits = u32::try_from(fraction.len()).ok()?;
            if digits == 0 || digits > 18 {
                return None;
            }
            let denominator = 10_i128.checked_pow(digits)?;
            let numerator = integer(fraction)?.checked_mul(scale)?;
            if numerator.checked_rem(denominator)? != 0 {
                return None;
            }
            whole.checked_add(numerator.checked_div(denominator)?)?
        }
    };
    i64::try_from(value).ok()
}

fn time_body(raw: &str) -> &str {
    raw.strip_suffix(" FAT")
        .or_else(|| raw.strip_suffix(" fat"))
        .or_else(|| raw.strip_suffix(" HT"))
        .or_else(|| raw.strip_suffix(" ht"))
        .or_else(|| raw.strip_suffix(['a', 'h', 'A', 'H']))
        .map_or(raw, str::trim)
}

fn time(raw: &str) -> Option<i64> {
    let count = raw.split(':').count();
    if count > 3 {
        return None;
    }
    raw.split(':')
        .enumerate()
        .try_fold(0_i64, |total, (index, part)| {
            if index.checked_add(1)? < count && part.contains('.') {
                return None;
            }
            let value = scaled(part, 1_000_000_000)?;
            if index > 0 && value >= 60_000_000_000 {
                return None;
            }
            total.checked_mul(60)?.checked_add(value)
        })
}

fn strip_unit<'a>(raw: &'a str, unit: &str) -> Option<&'a str> {
    let boundary = raw.len().checked_sub(unit.len())?;
    let suffix = raw.get(boundary..)?;
    if !suffix.eq_ignore_ascii_case(unit) {
        return None;
    }
    raw.get(..boundary).map(str::trim)
}

fn distance(raw: &str) -> Option<i64> {
    if let Some((feet, inches)) = raw.split_once(['\'', '′', '-']) {
        return imperial(feet, inches);
    }
    if let Some((feet, inches)) = ["ft", "FT", "feet", "Feet", "foot", "Foot"]
        .into_iter()
        .find_map(|unit| raw.split_once(unit))
    {
        return imperial(feet, inches);
    }
    if let Some(inches) = strip_inches(raw) {
        return scaled(inches, 25_400);
    }
    [
        ("centimetres", 10_000),
        ("centimeters", 10_000),
        ("cm", 10_000),
        ("millimetres", 1_000),
        ("millimeters", 1_000),
        ("mm", 1_000),
        ("metres", 1_000_000),
        ("meters", 1_000_000),
        ("m", 1_000_000),
    ]
    .into_iter()
    .find_map(|(unit, scale)| strip_unit(raw, unit).map(|number| (number, scale)))
    .map_or_else(
        || scaled(raw, 1_000_000),
        |(number, scale)| scaled(number, scale),
    )
}

fn strip_inches(raw: &str) -> Option<&str> {
    raw.strip_suffix(['"', '″']).map(str::trim).or_else(|| {
        ["inches", "inch", "in"]
            .into_iter()
            .find_map(|unit| strip_unit(raw, unit))
    })
}

fn imperial(feet: &str, inches: &str) -> Option<i64> {
    let feet = scaled(feet, 304_800)?;
    let inches = inches.trim();
    let inches = strip_inches(inches).map_or(inches, |value| value);
    let inches = if inches.is_empty() {
        0
    } else {
        scaled(inches, 25_400)?
    };
    if inches >= 304_800 {
        return None;
    }
    feet.checked_add(inches)
}

fn points(raw: &str) -> Option<i64> {
    if !raw.contains(',') {
        return scaled(raw, 100);
    }
    let whole = raw.split_once('.').map_or(raw, |(whole, _)| whole);
    let mut groups = whole.split(',');
    let first = groups.next()?;
    if first.is_empty() || first.len() > 3 || integer(first).is_none() {
        return None;
    }
    let whole = groups.try_fold(integer(first)?, |total, group| {
        if group.len() != 3 {
            return None;
        }
        total.checked_mul(1_000)?.checked_add(integer(group)?)
    })?;
    scale_decimal(
        whole,
        raw.split_once('.').map(|(_, fraction)| fraction),
        100,
    )
}

pub(super) fn timing(raw: &str, explicit: Option<&str>, measure: Measure) -> &'static str {
    if measure != Measure::Time {
        return "non_time";
    }
    match explicit.map(str::trim) {
        Some(value)
            if value.eq_ignore_ascii_case("fat")
                || value.eq_ignore_ascii_case("automatic")
                || value.eq_ignore_ascii_case("fully automatic") =>
        {
            "fat"
        }
        Some(value)
            if value.eq_ignore_ascii_case("hand")
                || value.eq_ignore_ascii_case("ht")
                || value.eq_ignore_ascii_case("hand timed") =>
        {
            "hand"
        }
        _ if raw.trim().ends_with(['a', 'A'])
            || raw.trim().ends_with(" FAT")
            || raw.trim().ends_with(" fat") =>
        {
            "fat"
        }
        _ if raw.trim().ends_with(['h', 'H'])
            || raw.trim().ends_with(" HT")
            || raw.trim().ends_with(" ht") =>
        {
            "hand"
        }
        _ => "unknown",
    }
}
