#[path = "support/records.rs"]
mod records;
mod support;

use anyhow::Result;
use track_scrapper_v2::database::Database;
use track_scrapper_v2::model::{Athlete, ParsedPage, School, Source};

async fn seed(
    database: &Database,
    id: &str,
    athlete_state: Option<&str>,
    school_state: Option<&str>,
    year: Option<i32>,
    needs_review: bool,
    result_school: bool,
) -> Result<()> {
    let school_id = format!("school-{id}");
    let school = School {
        id: school_id.clone(),
        name: format!("School {id}"),
        state: school_state.map(str::to_owned),
        ..School::default()
    };
    let athlete = Athlete {
        name: format!("Runner {id}"),
        state: athlete_state.map(str::to_owned),
        school_id: (!result_school).then_some(school_id.clone()),
        graduation_year: year,
        needs_review,
        ..records::athlete(id)
    };
    let performances = if result_school {
        let mut performance = records::performance("11.20");
        performance.athlete_id = id.to_owned();
        performance.school_id = Some(school_id);
        vec![performance]
    } else {
        Vec::new()
    };
    records::persist(
        database,
        &records::target(Source::AthleticNet, id),
        &ParsedPage {
            schools: vec![school],
            athletes: vec![athlete],
            performances,
            ..ParsedPage::default()
        },
    )
    .await
}

#[tokio::test]
async fn given_school_state_without_athlete_state_when_scoped_then_record_is_not_forced_to_review() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database, "11", None, Some("WI"), Some(2027), false, false).await?;
            seed(&database, "12", None, Some("WI"), Some(2027), true, false).await?;
            let records = database.athlete_records_for(&["WI".to_owned()], &[2027]).await?;
            assert_eq!(records.len(), 2);
            assert!(!records[0].needs_review);
            assert_eq!(records[0].schools[0].state.as_deref(), Some("WI"));
            assert!(records[1].needs_review);
            database.close().await;
            Ok(())
        })
    }).await
}

#[tokio::test]
async fn given_linked_school_states_when_scoped_then_school_and_cohort_determine_candidates() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(&database, "21", Some("OH"), Some(" wi "), Some(2027), false, false).await?;
            seed(&database, "22", Some("OH"), Some("WI"), Some(2027), false, true).await?;
            seed(&database, "23", Some("WI"), Some("OH"), Some(2027), false, false).await?;
            seed(&database, "24", Some("OH"), None, None, false, false).await?;
            seed(&database, "25", None, Some("WI"), Some(2028), false, false).await?;
            let records = database.athlete_records_for(&["WI".to_owned()], &[2027]).await?;
            assert_eq!(records.iter().map(|record| record.name.as_str()).collect::<Vec<_>>(),
                       vec!["Runner 21", "Runner 22", "Runner 24"]);
            assert_eq!(records[1].schools[0].state.as_deref(), Some("WI"));
            assert!(records[2].needs_review);
            let all = database.athlete_records_for(&[], &[]).await?;
            assert_eq!(all.len(), 5);
            database.close().await;
            Ok(())
        })
    }).await
}
