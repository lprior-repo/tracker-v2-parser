mod support;

use anyhow::Result;
use serde_json::Value;
use track_scrapper_v2::{
    database::Database,
    model::{
        Athlete, Capture, PageContext, PageKind, ParsedPage, Performance, Source, Sport, Target,
    },
};

fn observation() -> Performance {
    Performance {
        athlete_id: "athlete-1".into(),
        event: "100 Meters".into(),
        mark: "12.34".into(),
        reported_status: None,
        source_values: None,
        sport: Sport::Outdoor,
        result_id: Some("result-1".into()),
        source_row: Some("row-1".into()),
        school_id: None,
        meet_id: Some("meet-1".into()),
        meet_name: Some("Original meet".into()),
        date: Some("2026-04-01".into()),
        wind: Some(1.2),
        timing: Some("automatic".into()),
        course: Some("track".into()),
        is_personal_record: false,
        is_relay: false,
        season: Some(2026),
        grade: Some("11".into()),
        gender: Some("F".into()),
    }
}

async fn result(pool: &sqlx::PgPool) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('event', event, 'mark', raw_mark, 'meet_id', meet_id, 'meet_name', meet_name, 'date', meet_date, 'wind', wind, 'timing', timing, 'course', course, 'season', season, 'grade', grade, 'gender', gender, 'numeric', normalized_numeric, 'unit', normalized_unit, 'status', status, 'raw', raw) FROM results")
        .fetch_one(pool).await?)
}

#[tokio::test]
async fn sparse_observations_preserve_known_fields_and_populated_corrections_replace_them()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(
                Source::AthleticNet,
                PageKind::Athlete,
                "https://www.athletic.net/athlete/athlete-1".into(),
                PageContext::default(),
            );
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".into(),
                body: Vec::new(),
            };
            let rich = observation();
            let mut page = ParsedPage {
                athletes: vec![Athlete {
                    id: rich.athlete_id.clone(),
                    name: "Athlete".into(),
                    ..Athlete::default()
                }],
                performances: vec![rich.clone()],
                ..ParsedPage::default()
            };
            database.persist(None, &target, &capture, &page).await?;
            let original = result(&test.pool).await?;
            let mut sparse = rich;
            sparse.event.clear();
            sparse.mark.clear();
            sparse.source_row = None;
            sparse.meet_id = None;
            sparse.meet_name = Some(" ".into());
            sparse.date = None;
            sparse.wind = None;
            sparse.timing = None;
            sparse.course = None;
            sparse.season = None;
            sparse.grade = None;
            sparse.gender = None;
            page.performances = vec![sparse];
            database.persist(None, &target, &capture, &page).await?;
            assert_eq!(result(&test.pool).await?, original);
            let corrected = &mut page.performances[0];
            corrected.event = "100 Meters".into();
            corrected.mark = "DNF".into();
            corrected.meet_name = Some("Corrected meet".into());
            corrected.wind = Some(0.0);
            corrected.season = Some(2025);
            database.persist(None, &target, &capture, &page).await?;
            let updated = result(&test.pool).await?;
            assert_eq!(updated["mark"], "DNF");
            assert_eq!(updated["meet_name"], "Corrected meet");
            assert_eq!(updated["wind"].as_f64(), Some(0.0));
            assert_eq!(updated["season"], 2025);
            assert_eq!(updated["date"], "2026-04-01");
            assert_eq!(updated["numeric"], Value::Null);
            assert_eq!(updated["unit"], Value::Null);
            assert_eq!(updated["status"], "DNF");
            assert_eq!(updated["raw"]["meet_name"], "Corrected meet");
            assert_eq!(updated["raw"]["date"], "2026-04-01");
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn reported_dq_preserves_source_mark_but_never_counts_as_best() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(
                Source::AthleticNet,
                PageKind::Athlete,
                "https://www.athletic.net/athlete/athlete-1".into(),
                PageContext::default(),
            );
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".into(),
                body: Vec::new(),
            };
            let mut performance = observation();
            performance.reported_status = Some("DQ".into());
            performance.source_values = Some(serde_json::json!({"mark":"12.34","statusCode":"DQ"}));
            let mut page = ParsedPage {
                athletes: vec![Athlete {
                    id: performance.athlete_id.clone(),
                    name: "Athlete".into(),
                    ..Athlete::default()
                }],
                performances: vec![performance.clone()],
                ..ParsedPage::default()
            };
            database.persist(None, &target, &capture, &page).await?;
            let stored = result(&test.pool).await?;
            assert_eq!(stored["mark"], "12.34");
            assert_eq!(stored["status"], "DQ");
            assert!(stored["numeric"].is_null());
            assert_eq!(stored["raw"]["source_values"]["statusCode"], "DQ");
            assert!(
                track_scrapper_v2::marks::comparable_performance(
                    Source::AthleticNet,
                    &target.url,
                    &performance
                )
                .is_none()
            );
            let records = database.athlete_records().await?;
            let evidence = records
                .first()
                .and_then(|record| record.performances.first())
                .ok_or_else(|| anyhow::anyhow!("missing persisted evidence"))?;
            assert_eq!(evidence.performance.reported_status.as_deref(), Some("DQ"));
            assert_eq!(evidence.performance.mark, "12.34");
            assert!(track_scrapper_v2::marks::comparable(evidence).is_none());
            performance.reported_status = None;
            page.performances = vec![performance];
            database.persist(None, &target, &capture, &page).await?;
            let corrected = result(&test.pool).await?;
            assert_eq!(corrected["status"], "recorded");
            assert!(corrected["raw"].get("reported_status").is_none());
            assert!(!corrected["numeric"].is_null());
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn idless_rows_at_same_position_keep_separate_persisted_evidence() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(Source::MileSplit, PageKind::Results, "https://al.milesplit.com/api/v1/meets/725218/performances?isMeetPro=0".into(), PageContext {
                subject_id: Some("725218".into()), result_set_ids: vec!["1266814".into()], sport: Some(Sport::Outdoor), ..PageContext::default()
            });
            let capture = Capture { url: target.url.clone(), status: 200, content_type: "application/json".into(), body: Vec::new() };
            let original: Value = serde_json::from_str(include_str!("../fixtures/milesplit/performances-al-725218.json"))?;
            let mut first = original["data"][0].clone();
            first["id"] = Value::Null;
            first["statusCode"] = serde_json::json!("DQ");
            let mut second = first.clone();
            second["mark"] = serde_json::json!("12.41");
            for row in [first, second] {
                let body = serde_json::json!({"_meta":{"status_code":200}, "data":[row]}).to_string();
                let page = track_scrapper_v2::sources::milesplit::parse(&target, &body)?;
                database.persist(None, &target, &capture, &page).await?;
            }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE source = 'milesplit'").fetch_one(&test.pool).await?;
            assert_eq!(count, 2);
            let noncomparable: i64 = sqlx::query_scalar("SELECT count(*) FROM results WHERE source = 'milesplit' AND status = 'DQ' AND normalized_numeric IS NULL").fetch_one(&test.pool).await?;
            assert_eq!(noncomparable, 2);
            database.close().await;
            Ok(())
        })
    }).await
}
