#[path = "reviewed_result_profile_conflicts/fixture.rs"]
mod fixture;
mod support;

use anyhow::Result;
use fixture::{
    ALIAS, PRIOR_OWNED_ID, PROFILE_ID, PROFILE_NAME, ResultFixture, approve_alias, observe_profile,
};
use serde_json::Value;
use track_scrapper_v2::database::Database;

async fn claims(database: &Database) -> Result<Vec<(String, Value)>> {
    Ok(sqlx::query_as(
        "SELECT r.source_result_id,to_jsonb(l) FROM reviewed_result_links l \
         JOIN results r USING (source_result_key) ORDER BY r.source_result_id",
    )
    .fetch_all(database.pool())
    .await?)
}

async fn profile(database: &Database) -> Result<(String, String, String, String, String, bool)> {
    Ok(sqlx::query_as(
        "SELECT p.name,a.name,p.gender,a.gender,p.details->>'gender',a.needs_review \
         FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id \
         WHERE p.source='milesplit' AND p.source_id=$1",
    )
    .bind(PROFILE_ID)
    .fetch_one(database.pool())
    .await?)
}

fn assert_reused(report: &Value) {
    assert_eq!(report["links_added"], 0);
    assert_eq!(report["links_already_present"], 1);
}

#[tokio::test]
async fn given_unchanged_profile_when_review_and_capture_are_replayed_then_one_claim_remains_valid()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            observe_profile(&database, PROFILE_ID, PROFILE_NAME, "F").await?;
            let result = ResultFixture::new(152, "23:03.4");
            result.persist(&database).await?;
            let first = result.import(test, "original-review").await?;
            assert_eq!(first["links_added"], 1);
            assert_eq!(first["links_already_present"], 0);
            let before = claims(&database).await?;
            assert_eq!(
                before
                    .iter()
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>(),
                vec![result.result_id.as_str()]
            );
            observe_profile(&database, PROFILE_ID, PROFILE_NAME, "F").await?;
            result.persist(&database).await?;
            assert_eq!(result.import(test, "original-review").await?, first);
            assert_reused(&result.import(test, "independent-readmission").await?);
            assert_eq!(claims(&database).await?, before);
            result.assert_exported(&database).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_latest_male_profile_when_new_girls_link_is_reviewed_then_admission_is_rejected_without_invalidating_the_existing_claim()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            observe_profile(&database, PROFILE_ID, PROFILE_NAME, "F").await?;
            let approved = ResultFixture::new(152, "23:03.4");
            approved.persist(&database).await?;
            assert_eq!(
                approved.import(test, "original-review").await?["links_added"],
                1
            );
            let before = claims(&database).await?;
            assert_eq!(before.len(), 1);
            observe_profile(&database, PROFILE_ID, PROFILE_NAME, "M").await?;
            assert_eq!(
                profile(&database).await?,
                (
                    PROFILE_NAME.to_owned(),
                    PROFILE_NAME.to_owned(),
                    "F".to_owned(),
                    "F".to_owned(),
                    "M".to_owned(),
                    true,
                )
            );
            let candidate = ResultFixture::new(153, "23:18.2");
            approved
                .persist_capture(&database, Some(&candidate), false)
                .await?;
            let Err(error) = candidate.import(test, "conflicting-review").await else {
                anyhow::bail!("the conflicting review unexpectedly succeeded");
            };
            assert!(
                format!("{error:#}")
                    .contains("reviewed result, profile, and canonical athlete genders conflict")
            );
            assert_eq!(claims(&database).await?, before);
            let owners: (Option<i64>, Option<i64>) = sqlx::query_as(
                "SELECT athlete_source_id,athlete_id FROM results WHERE source_result_id=$1",
            )
            .bind(&candidate.result_id)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(owners, (None, None));
            approved.assert_exported(&database).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_approved_identity_alias_when_latest_profile_uses_alias_then_review_remains_valid_and_readmission_succeeds()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            observe_profile(&database, PROFILE_ID, PROFILE_NAME, "F").await?;
            approve_alias(&database, test).await?;
            let result = ResultFixture::new(152, "23:03.4");
            result.persist(&database).await?;
            assert_eq!(
                result.import(test, "original-review").await?["links_added"],
                1
            );
            let before = claims(&database).await?;
            observe_profile(&database, PROFILE_ID, ALIAS, "F").await?;
            let names: (String, String, String, i64) = sqlx::query_as(
                "SELECT p.name,p.details->>'name',a.name,(SELECT count(*) FROM athletes) \
                 FROM athlete_sources p JOIN athletes a ON a.id=p.athlete_id WHERE p.source_id=$1",
            )
            .bind(PROFILE_ID)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(
                names,
                (
                    PROFILE_NAME.to_owned(),
                    ALIAS.to_owned(),
                    PROFILE_NAME.to_owned(),
                    1
                )
            );
            assert_reused(&result.import(test, "alias-readmission").await?);
            assert_eq!(claims(&database).await?, before);
            result.assert_exported(&database).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_unrelated_result_revision_when_profile_identity_is_unchanged_then_readmission_succeeds()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            observe_profile(&database, PROFILE_ID, PROFILE_NAME, "F").await?;
            let result = ResultFixture::new(152, "23:03.4");
            result.persist_capture(&database, None, true).await?;
            assert_eq!(result.import(test, "original-review").await?["links_added"], 1);
            let before = claims(&database).await?;
            assert!(!profile(&database).await?.5);
            result.persist(&database).await?;
            let warning: String = sqlx::query_scalar(
                "SELECT raw#>>'{source_values,revision_status}' FROM results WHERE source_result_id=$1",
            )
            .bind(PRIOR_OWNED_ID)
            .fetch_one(database.pool())
            .await?;
            assert_eq!(warning, "unverified_prior_revision");
            assert_eq!(profile(&database).await?, (
                PROFILE_NAME.to_owned(), PROFILE_NAME.to_owned(), "F".to_owned(), "F".to_owned(), "F".to_owned(), true,
            ));
            assert_reused(&result.import(test, "warning-readmission").await?);
            assert_eq!(claims(&database).await?, before);
            result.assert_exported(&database).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}
