use track_scrapper_v2::marks::comparable_performance;
use track_scrapper_v2::model::{Performance, Source, Sport};

fn performance() -> Performance {
    Performance {
        athlete_id: "123".to_owned(),
        event: "5k".to_owned(),
        mark: "15:00".to_owned(),
        reported_status: None,
        source_values: None,
        sport: Sport::CrossCountry,
        result_id: None,
        source_row: Some("resultsXC:1".to_owned()),
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
    }
}

#[test]
fn given_unknown_courses_when_source_rows_differ_then_comparison_keys_stay_separate()
-> anyhow::Result<()> {
    let first = performance();
    let mut second = first.clone();
    second.source_row = Some("resultsXC:2".to_owned());
    let key = |item: &Performance| {
        comparable_performance(Source::AthleticNet, "https://example.com/athlete/123", item)
            .map(|mark| mark.column)
            .ok_or_else(|| anyhow::anyhow!("cross-country mark did not classify"))
    };
    assert_ne!(key(&first)?, key(&second)?);
    assert_eq!(key(&first)?, key(&first.clone())?);
    second.source_row = None;
    assert_ne!(key(&first)?, key(&second)?);
    Ok(())
}

#[test]
fn given_unknown_courses_when_result_or_page_differs_then_comparison_keys_stay_separate()
-> anyhow::Result<()> {
    let first = performance();
    let mut second = first.clone();
    second.result_id = Some("result-2".to_owned());
    let key = |url: &str, item: &Performance| {
        comparable_performance(Source::AthleticNet, url, item)
            .map(|mark| mark.column)
            .ok_or_else(|| anyhow::anyhow!("cross-country mark did not classify"))
    };
    assert_ne!(key("page-a", &first)?, key("page-a", &second)?);
    assert_ne!(key("page-a", &first)?, key("page-b", &first)?);
    Ok(())
}

#[test]
fn given_named_course_when_source_rows_differ_then_marks_remain_comparable() -> anyhow::Result<()> {
    let mut first = performance();
    first.course = Some("Riverside".to_owned());
    let mut second = first.clone();
    second.source_row = Some("resultsXC:2".to_owned());
    let key = |item: &Performance| {
        comparable_performance(Source::AthleticNet, "page-a", item)
            .map(|mark| mark.column)
            .ok_or_else(|| anyhow::anyhow!("cross-country mark did not classify"))
    };
    assert_eq!(key(&first)?, key(&second)?);
    Ok(())
}
