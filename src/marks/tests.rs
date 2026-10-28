use super::{Measure, comparable, parse_mark};
use crate::model::{EvidencedPerformance, Performance, Source, Sport};

pub(crate) fn evidence(event: &str, mark: &str) -> EvidencedPerformance {
    EvidencedPerformance {
        source: Source::AthleticNet,
        url: "https://www.athletic.net/athlete/123".to_owned(),
        performance: Performance {
            athlete_id: "123".to_owned(),
            event: event.to_owned(),
            mark: mark.to_owned(),
            reported_status: None,
            source_values: None,
            sport: Sport::Outdoor,
            result_id: None,
            source_row: None,
            school_id: None,
            meet_id: None,
            meet_name: None,
            date: None,
            wind: None,
            timing: None,
            course: None,
            is_personal_record: false,
            is_relay: false,
            season: None,
            grade: None,
            gender: None,
        },
    }
}

#[test]
fn given_rounded_hurdle_alternatives_when_compared_then_literal_specifications_stay_distinct() {
    for (inches, metric) in [(39, "0.991"), (36, "0.914"), (33, "0.838"), (30, "0.762")] {
        let label = format!("110m Hurdles ({inches}\" / {metric}m)");
        let dual = comparable(&evidence(&label, "15.00a")).expect("published dual units");
        assert_eq!(dual.value, 15_000_000_000);
        assert!(dual.column.ends_with(&format!("::spec={label}")));
        assert_ne!(
            Some(dual.clone()),
            comparable(&evidence(&format!("110m Hurdles {inches}in"), "15.00a"))
        );
        assert_ne!(
            Some(dual),
            comparable(&evidence(&format!("110m Hurdles {metric}m"), "15.00a"))
        );
    }
    for label in [
        "110m Hurdles (39\" / unknown)",
        "110m Hurdles (39\" / 0m)",
        "110m Hurdles (39\" / 1.2m)",
        "110m Hurdles (39\" / 0.991m / 42\")",
        "110m Hurdles (39\" / 42\")",
        "Shot Put (12lb / 4kg)",
    ] {
        assert!(comparable(&evidence(label, "15.00a")).is_none(), "{label}");
    }
}

#[test]
fn given_exact_four_kilometre_xc_when_compared_then_distance_and_course_remain_distinct() {
    let key = |event: &str, course: &str| {
        let mut item = evidence(event, "15:00");
        item.performance.sport = Sport::CrossCountry;
        item.performance.course = Some(course.to_owned());
        comparable(&item)
    };
    let four_k = key("Cross Country 4000m", "Course A").expect("exact 4000m");
    assert_eq!(four_k.value, 900_000_000_000);
    assert_eq!(Some(four_k.clone()), key("4k", "Course A"));
    assert_ne!(Some(four_k.clone()), key("5k", "Course A"));
    assert_ne!(Some(four_k), key("4k", "Course B"));
    for unknown in ["4001m", "4.1k", "about 4k", "4k short course"] {
        assert!(key(unknown, "Course A").is_none(), "{unknown}");
    }
}

#[test]
fn given_measure_types_when_comparing_then_time_minimizes_and_field_maximizes() {
    assert!(Measure::Time.better(10_400_000_000, 10_500_000_000));
    assert!(Measure::Distance.better(6_100_000, 6_000_000));
    assert!(Measure::Points.better(510_000, 500_000));
    assert!(!Measure::Time.better(10_500_000_000, 10_400_000_000));
}

#[test]
fn given_imperial_and_metric_when_parsing_then_units_are_correct() -> anyhow::Result<()> {
    let imperial = parse_mark("20-6", Measure::Distance)
        .ok_or_else(|| anyhow::anyhow!("imperial mark did not parse"))?;
    assert_eq!(imperial, 6_248_400);
    assert_eq!(parse_mark("6.25m", Measure::Distance), Some(6_250_000));
    assert_eq!(parse_mark("20' 6\"", Measure::Distance), Some(imperial));
    assert_eq!(parse_mark("1:02.34h", Measure::Time), Some(62_340_000_000));
    assert_eq!(parse_mark("10.41a", Measure::Time), Some(10_410_000_000));
    Ok(())
}

#[test]
fn given_failed_or_nonfinite_marks_when_parsing_then_no_pr_is_invented() {
    [
        "DNS",
        "DNF",
        "DQ",
        "NaN",
        "inf",
        "-1",
        "0",
        "1:60",
        "10.4garbage",
    ]
    .iter()
    .for_each(|raw| assert_eq!(parse_mark(raw, Measure::Time), None, "{raw}"));
    assert!(comparable(&evidence("400m Relay", "42.0")).is_none());
    assert!(comparable(&evidence("Unknown Run", "42.0")).is_none());
}

#[test]
fn given_different_events_and_contexts_when_classifying_then_columns_stay_distinct() {
    let key = |item: &EvidencedPerformance| {
        let mark = comparable(item);
        assert!(mark.is_some(), "valid scenario mark must classify");
        mark.map(|mark| mark.column)
    };
    assert_ne!(
        key(&evidence("Mile", "4:20")),
        key(&evidence("1600m", "4:20"))
    );
    assert_ne!(
        key(&evidence("100m", "10.4a")),
        key(&evidence("100m", "10.4h"))
    );
    let mut legal = evidence("100m", "10.4a");
    legal.performance.wind = Some(2.0);
    let mut assisted = legal.clone();
    assisted.performance.wind = Some(2.1);
    assert_ne!(key(&legal), key(&assisted));
    assert_ne!(key(&legal), key(&evidence("100m", "10.4a")));
    assert_ne!(
        key(&evidence("Shot Put 12lb", "10m")),
        key(&evidence("Shot Put 16lb", "10m"))
    );
    assert_ne!(
        key(&evidence("110m Hurdles 39in", "15a")),
        key(&evidence("110m Hurdles 42in", "15a"))
    );
    let mut xc = evidence("2 Miles", "15:00");
    xc.performance.sport = Sport::CrossCountry;
    let mut other = xc.clone();
    other.performance.event = "3 Miles".to_owned();
    assert_ne!(key(&xc), key(&other));
    other.performance.event = "5k".to_owned();
    assert_ne!(key(&xc), key(&other));
    other = xc.clone();
    other.performance.course = Some("Actual named course".to_owned());
    assert_ne!(key(&xc), key(&other));
    other = xc.clone();
    other.source = Source::MileSplit;
    assert_ne!(key(&xc), key(&other));
    other = xc.clone();
    other.performance.mark = "14:59".to_owned();
    assert_ne!(
        key(&xc),
        key(&other),
        "unknown courses must not collapse on a shared athlete page"
    );
}

#[test]
fn given_unknown_implement_or_hurdle_height_when_classifying_then_marks_are_not_comparable() {
    [
        ("Shot Put", "10m"),
        ("Discus", "30m"),
        ("Javelin", "30m"),
        ("Hammer", "30m"),
        ("Weight Throw", "10m"),
        ("55m Hurdles", "9a"),
        ("60m Hurdles", "10a"),
        ("100m Hurdles", "15a"),
        ("110m Hurdles", "15a"),
        ("300m Hurdles", "40a"),
        ("400m Hurdles", "55a"),
    ]
    .iter()
    .for_each(|(event, mark)| {
        assert!(comparable(&evidence(event, mark)).is_none(), "{event}");
    });
    assert!(comparable(&evidence("Long Jump", "6m")).is_some());
    assert!(comparable(&evidence("400m", "55a")).is_some());
}

#[test]
fn given_event_suffix_when_classifying_then_only_explicit_dimensions_are_comparable() {
    [
        ("Shot Put Final", "10m"),
        ("Shot Put 12", "10m"),
        ("Shot Put 0lb", "10m"),
        ("Discus Varsity", "30m"),
        ("110m Hurdles Final", "15a"),
        ("110m Hurdles 39", "15a"),
        ("110m Hurdles 0in", "15a"),
    ]
    .iter()
    .for_each(|(event, mark)| {
        assert!(comparable(&evidence(event, mark)).is_none(), "{event}");
    });
    [
        ("Shot Put 12lb", "10m"),
        ("Discus 1.6kg", "30m"),
        ("Javelin 800g", "30m"),
        ("110m Hurdles 39in", "15a"),
        ("100m Hurdles 84cm", "15a"),
    ]
    .iter()
    .for_each(|(event, mark)| {
        assert!(comparable(&evidence(event, mark)).is_some(), "{event}");
    });
}

#[test]
fn given_fractional_imperial_marks_when_normalizing_then_no_fraction_or_unit_is_lost() {
    [
        "20-03.50",
        "20'3.5\"",
        "20ft3.5in",
        "20 ft 3.5 in",
        "20′ 3.5″",
        "6.1849 m",
    ]
    .iter()
    .for_each(|mark| {
        assert_eq!(
            parse_mark(mark, Measure::Distance),
            Some(6_184_900),
            "{mark}"
        )
    });
    assert_eq!(parse_mark("1.5 cm", Measure::Distance), Some(15_000));
    assert_eq!(parse_mark("1.5 mm", Measure::Distance), Some(1_500));
    assert_eq!(parse_mark("0.125in", Measure::Distance), Some(3_175));
    assert_eq!(parse_mark("20-12.50", Measure::Distance), None);
    assert_eq!(parse_mark("0.00001in", Measure::Distance), None);
}

#[test]
fn given_exact_unit_boundaries_when_parsing_then_precision_overflow_and_grouping_are_checked() {
    assert_eq!(parse_mark("44.88", Measure::Time), Some(44_880_000_000));
    assert_eq!(parse_mark("111.47", Measure::Time), Some(111_470_000_000));
    assert_eq!(
        parse_mark("1:51.47 FAT", Measure::Time),
        Some(111_470_000_000)
    );
    assert_eq!(parse_mark("0.000000001", Measure::Time), Some(1));
    assert_eq!(
        parse_mark("9223372036.854775807", Measure::Time),
        Some(i64::MAX)
    );
    assert_eq!(parse_mark("9223372036.854775808", Measure::Time), None);
    assert_eq!(parse_mark("44.8812345678", Measure::Time), None);
    assert_eq!(parse_mark("1.2.3", Measure::Time), None);
    assert_eq!(parse_mark("1.5:30", Measure::Time), None);
    assert_eq!(parse_mark("1:00:60", Measure::Time), None);
    assert_eq!(parse_mark("7,234.56", Measure::Points), Some(723_456));
    assert_eq!(parse_mark("7234.5", Measure::Points), Some(723_450));
    assert_eq!(parse_mark("7234.567", Measure::Points), None);
    assert_eq!(parse_mark("7,23.45", Measure::Points), None);
    assert_eq!(parse_mark("0", Measure::Points), None);
    assert_eq!(parse_mark("0", Measure::Distance), None);
}

#[test]
fn given_storage_normalization_when_classifying_then_units_specs_and_relay_exclusion_agree() {
    let mut mark = evidence("Shot Put 12lb", "20ft3.5in");
    assert_eq!(
        super::normalized_performance(&mark.performance),
        Some((
            6_184_900,
            Measure::Distance,
            Some("Shot Put 12lb".to_owned())
        ))
    );
    assert_eq!(Measure::Time.unit(), "ns");
    assert_eq!(Measure::Distance.unit(), "um");
    assert_eq!(Measure::Points.unit(), "centipoints");
    mark.performance.is_relay = true;
    assert_eq!(super::normalized_performance(&mark.performance), None);
    assert_eq!(super::normalized_mark("4x100m", "44.88"), None);
}
