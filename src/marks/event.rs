use std::borrow::Cow;

use super::Measure;

pub(super) struct Event {
    pub label: String,
    pub measure: Measure,
    pub wind_sensitive: bool,
    pub relay: bool,
    pub specification: Option<String>,
}

pub(super) fn resolve(raw: &str) -> Option<Event> {
    let normalized = normalize(raw);
    let label = normalized.as_str();
    let relay = label.contains("relay")
        || label.contains("4x")
        || label.contains('×')
        || matches!(
            label,
            "smr" | "dmr" | "smed" | "dmed" | "sprintmedley" | "distancemedley"
        );
    if relay {
        return Some(Event {
            label: normalized,
            measure: Measure::Time,
            wind_sensitive: false,
            relay,
            specification: None,
        });
    }
    let classified = without_categories(label);
    let stripped = strip_qualifiers(&classified);
    let (canonical, measure) = direct(stripped).or_else(|| specified_field(stripped))?;
    let specification =
        (direct(stripped).is_none() || classified != label).then(|| raw.trim().to_owned());
    let wind_sensitive = matches!(
        canonical.as_str(),
        "100m" | "200m" | "100mh" | "110mh" | "long_jump" | "triple_jump"
    );
    Some(Event {
        label: canonical,
        measure,
        wind_sensitive,
        relay: false,
        specification,
    })
}

pub(super) fn has_comparable_specification(raw: &str, canonical: &str) -> bool {
    let (prefixes, units): (&[&str], &[&str]) = match canonical {
        "shot_put" => (&["shotput"], &["kg", "kgs", "g", "lb", "lbs", "oz"]),
        "discus" => (&["discus"], &["kg", "kgs", "g", "lb", "lbs", "oz"]),
        "javelin" => (&["javelin"], &["kg", "kgs", "g", "lb", "lbs", "oz"]),
        "hammer" => (&["hammer"], &["kg", "kgs", "g", "lb", "lbs", "oz"]),
        "weight_throw" => (&["weightthrow"], &["kg", "kgs", "g", "lb", "lbs", "oz"]),
        "55mh" => (
            &["55mhurdles", "55mh"],
            &["inches", "inch", "in", "cm", "m", "\"", "″"],
        ),
        "60mh" => (
            &["60mhurdles", "60mh"],
            &["inches", "inch", "in", "cm", "m", "\"", "″"],
        ),
        "100mh" => (
            &["100mhurdles", "100mh"],
            &["inches", "inch", "in", "cm", "m", "\"", "″"],
        ),
        "110mh" => (
            &["110mhurdles", "110mh"],
            &["inches", "inch", "in", "cm", "m", "\"", "″"],
        ),
        "300mh" => (
            &["300mhurdles", "300mh"],
            &["inches", "inch", "in", "cm", "m", "\"", "″"],
        ),
        "400mh" => (
            &["400mhurdles", "400mh"],
            &["inches", "inch", "in", "cm", "m", "\"", "″"],
        ),
        _ => return true,
    };
    let normalized = normalize(raw);
    let classified = without_categories(&normalized);
    let label = strip_qualifiers(&classified);
    prefixes.iter().any(|prefix| {
        label.strip_prefix(prefix).is_some_and(|suffix| {
            let specification = suffix.trim_matches(['(', ')']);
            units.iter().any(|unit| {
                specification
                    .strip_suffix(unit)
                    .is_some_and(positive_decimal)
            }) || (canonical.ends_with("mh") && rounded_hurdle_units(specification).is_some())
        })
    })
}

fn rounded_hurdle_units(specification: &str) -> Option<()> {
    let (imperial, metric) = specification.split_once('/')?;
    let inches = ["inches", "inch", "in", "\"", "″"]
        .into_iter()
        .find_map(|unit| imperial.strip_suffix(unit))?;
    let metres = metric.strip_suffix('m')?;
    if !positive_decimal(inches)
        || !positive_decimal(metres)
        || metres.split_once('.')?.1.len() != 3
    {
        return None;
    }
    let imperial = super::parse_mark(imperial, Measure::Distance)?;
    let metric = super::parse_mark(metric, Measure::Distance)?;
    (imperial.checked_add(500)?.checked_div(1_000)? == metric.checked_div(1_000)?).then_some(())
}

fn without_categories(label: &str) -> Cow<'_, str> {
    if !label.contains("wheelchair") && !label.contains("seated") {
        return Cow::Borrowed(label);
    }
    Cow::Owned(
        label
            .replace("wheelchair", "")
            .replace("seated", "")
            .replace("()", ""),
    )
}

fn positive_decimal(number: &str) -> bool {
    let mut parts = number.split('.');
    let whole = parts.next().map_or("", |part| part);
    let fraction = parts.next();
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        && parts.next().is_none()
        && number.bytes().any(|byte| matches!(byte, b'1'..=b'9'))
}

fn normalize(raw: &str) -> String {
    raw.to_lowercase()
        .replace("metres", "m")
        .replace("meters", "m")
        .replace("metre", "m")
        .replace("meter", "m")
        .chars()
        .filter(|character| !character.is_whitespace() && !matches!(character, '-' | '_' | ','))
        .collect()
}

fn strip_qualifiers(label: &str) -> &str {
    let prefixes = [
        "varsity",
        "juniorvarsity",
        "girls",
        "boys",
        "women",
        "men",
        "female",
        "male",
        "freshman",
        "freshmen",
        "jv",
        "crosscountry",
        "xc",
    ];
    let label = prefixes
        .iter()
        .chain(prefixes.iter())
        .fold(label, |remaining, prefix| {
            remaining
                .strip_prefix(*prefix)
                .map_or(remaining, |rest| rest)
        });
    label
        .strip_suffix("run")
        .or_else(|| label.strip_suffix("dash"))
        .map_or(label, |rest| rest)
}

fn direct(label: &str) -> Option<(String, Measure)> {
    let canonical = match label {
        "55m" | "60m" | "100m" | "200m" | "300m" | "400m" | "500m" | "600m" | "800m" | "1000m"
        | "1500m" | "1600m" | "2000m" | "3000m" | "3200m" | "4000m" | "5000m" | "10000m" => label,
        "1k" => "1000m",
        "2k" => "2000m",
        "3k" => "3000m",
        "4k" => "4000m",
        "5k" => "5000m",
        "10k" => "10000m",
        "mile" | "1mile" | "1mi" => "mile",
        "2mile" | "2miles" | "twomile" | "twomiles" | "2mi" => "2mi",
        "3mile" | "3miles" | "3mi" => "3mi",
        "55mh" | "55h" | "55mhurdles" => "55mh",
        "60mh" | "60h" | "60mhurdles" => "60mh",
        "100mh" | "100h" | "100mhurdles" => "100mh",
        "110mh" | "110h" | "110mhurdles" | "110mhhurdles" => "110mh",
        "300mh" | "300h" | "300mhurdles" | "300hurdles" => "300mh",
        "400mh" | "400h" | "400mhurdles" | "400hurdles" => "400mh",
        "2000msteeplechase" | "2ksteeplechase" | "2000msteeple" => "2000msc",
        "3000msteeplechase" | "3ksteeplechase" | "3000msteeple" => "3000msc",
        _ => return field(label),
    };
    Some((canonical.to_owned(), Measure::Time))
}

fn field(label: &str) -> Option<(String, Measure)> {
    let canonical = match label {
        "highjump" | "hj" => "high_jump",
        "longjump" | "lj" => "long_jump",
        "triplejump" | "tj" => "triple_jump",
        "polevault" | "pv" => "pole_vault",
        "shotput" | "shot" | "sp" => "shot_put",
        "discus" | "disc" | "dt" => "discus",
        "javelin" | "jav" | "jt" => "javelin",
        "hammer" | "ht" => "hammer",
        "weightthrow" | "wt" => "weight_throw",
        "pentathlon" | "heptathlon" | "decathlon" => {
            return Some((label.to_owned(), Measure::Points));
        }
        _ => return None,
    };
    Some((canonical.to_owned(), Measure::Distance))
}

fn specified_field(label: &str) -> Option<(String, Measure)> {
    [
        "shotput",
        "discus",
        "javelin",
        "hammer",
        "weightthrow",
        "highjump",
        "longjump",
        "triplejump",
        "polevault",
    ]
    .into_iter()
    .find_map(|prefix| {
        label
            .strip_prefix(prefix)
            .filter(|rest| !rest.is_empty())
            .and_then(|_| field(prefix))
    })
    .or_else(|| {
        ["55mh", "60mh", "100mh", "110mh", "300mh", "400mh"]
            .into_iter()
            .find_map(|prefix| {
                label
                    .strip_prefix(prefix)
                    .filter(|rest| !rest.is_empty())
                    .and_then(|_| direct(prefix))
            })
    })
}
