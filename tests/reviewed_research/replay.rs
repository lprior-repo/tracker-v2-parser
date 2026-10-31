use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, Coach, PageContext, PageKind, ParsedPage, School, Source, Sport, Target},
};

use crate::{reviewed_research::support as fixture, support::with_schema};

#[tokio::test]
async fn given_manual_research_when_crawler_replays_then_reviewed_facts_and_audit_survive()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let mut address = fixture::address("school-1", school_id);
            address["postal_code"] = Value::Null;
            let body = fixture::envelope(vec![address, fixture::coach(school_id)]);
            let (workspace, spec, _) = fixture::build(test, "research-replay", body)?;
            fixture::apply(test, &workspace, &spec).await?;

            replay(test, "Madison", "2 Oak Street", Some("53703")).await?;
            let first: (String, String, Value) = sqlx::query_as(
                "SELECT address,postal_code,address_provenance FROM schools WHERE id=$1",
            )
            .bind(school_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(first.0, "2 Oak Street");
            assert_eq!(first.1, "53703");
            assert_eq!(first.2.pointer("/manual_research_conflicts"), None);
            let raw: Value = sqlx::query_scalar("SELECT raw FROM contacts WHERE school_id=$1")
                .bind(school_id)
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(
                raw.pointer("/manual_research/0/researcher"),
                Some(&json!("gpt-6-luna"))
            );

            replay(test, "Monona", "9 Pine Street", Some("53704")).await?;
            let final_school: (String, String, String, Value) = sqlx::query_as(
                "SELECT name,city,address, address_provenance FROM schools WHERE id=$1",
            )
            .bind(school_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                (
                    final_school.0.as_str(),
                    final_school.1.as_str(),
                    final_school.2.as_str()
                ),
                ("Test High School", "Madison", "2 Oak Street")
            );
            let conflicts = final_school
                .3
                .pointer("/manual_research_conflicts")
                .and_then(Value::as_array)
                .context("conflicting crawler claims must be retained for review")?;
            assert!(
                conflicts
                    .iter()
                    .any(|item| item["field"] == "address" && item["needs_review"] == true)
            );
            assert!(
                conflicts
                    .iter()
                    .any(|item| item["field"] == "city" && item["needs_review"] == true)
            );
            let conflict_count = conflicts.len();
            replay(test, "Monona", "9 Pine Street", Some("53704")).await?;
            let repeated_conflicts: Value = sqlx::query_scalar(
                "SELECT address_provenance->'manual_research_conflicts' FROM schools WHERE id=$1",
            )
            .bind(school_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(
                repeated_conflicts.as_array().map(Vec::len),
                Some(conflict_count)
            );
            let raw: Value = sqlx::query_scalar("SELECT raw FROM contacts WHERE school_id=$1")
                .bind(school_id)
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(
                raw.pointer("/manual_research/0/researcher"),
                Some(&json!("gpt-6-luna"))
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_unknown_reviewed_city_and_state_when_crawler_replays_then_identity_can_be_enriched()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            sqlx::query("UPDATE schools SET city=NULL,state='' WHERE id=$1")
                .bind(school_id)
                .execute(&test.pool)
                .await?;
            let mut address = fixture::address("school-1", school_id);
            address["expected_city"] = Value::Null;
            address["expected_state"] = json!("");
            let (workspace, spec, _) = fixture::build(
                test,
                "research-replay-unknown-identity",
                fixture::envelope(vec![address]),
            )?;
            fixture::apply(test, &workspace, &spec).await?;

            replay(test, "Madison", "2 Oak Street", Some("53703")).await?;
            let row: (Option<String>, Option<String>, Option<String>, Value) = sqlx::query_as(
                "SELECT city,state,address,address_provenance FROM schools WHERE id=$1",
            )
            .bind(school_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(row.0.as_deref(), Some("Madison"));
            assert_eq!(row.1.as_deref(), Some("WI"));
            assert_eq!(row.2.as_deref(), Some("2 Oak Street"));
            assert_eq!(row.3.pointer("/manual_research_conflicts"), None);
            Ok(())
        })
    })
    .await
}

async fn replay(
    test: &crate::support::Scenario,
    city: &str,
    address: &str,
    postal_code: Option<&str>,
) -> Result<()> {
    let database = Database::connect(&test.url).await?;
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        "https://school.example.edu/staff".to_owned(),
        PageContext {
            school_id: Some("school-1".to_owned()),
            school_name: Some("Test High School".to_owned()),
            state: Some("WI".to_owned()),
            ..PageContext::default()
        },
    );
    let page = ParsedPage {
        schools: vec![School {
            id: "school-1".to_owned(),
            name: "Test High School".to_owned(),
            city: Some(city.to_owned()),
            state: Some("WI".to_owned()),
            address: Some(address.to_owned()),
            postal_code: postal_code.map(str::to_owned),
            ..School::default()
        }],
        coaches: vec![Coach {
            school_id: "school-1".to_owned(),
            name: Some("Jordan Coach".to_owned()),
            role: "Head Girls Track Coach".to_owned(),
            email: Some("jordan@example.edu".to_owned()),
            sport: Some(Sport::Outdoor),
            review_notes: Vec::new(),
        }],
        ..ParsedPage::default()
    };
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".to_owned(),
        body: serde_json::to_vec(&page)?,
    };
    database.persist(None, &target, &capture, &page).await?;
    database.close().await;
    Ok(())
}
