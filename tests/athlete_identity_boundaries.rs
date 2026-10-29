#[path = "support/identity_review.rs"]
mod identity;
mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Athlete, Capture, PageContext, PageKind, ParsedPage, Source, Target},
    sources,
};

async fn snapshot(test: &support::Scenario) -> Result<Value> {
    sqlx::query_scalar("SELECT jsonb_build_object('athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a),'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p),'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),'queue',(SELECT jsonb_agg(to_jsonb(q) ORDER BY id) FROM scrape_pages q))")
        .fetch_one(&test.pool).await.map_err(Into::into)
}

#[tokio::test]
async fn given_invalid_or_stale_user_reviews_when_imported_then_every_database_row_is_unchanged()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            identity::seed(test).await?;
            let valid = identity::manifest(test).await?;
            let before = snapshot(test).await?;
            let mut stale = valid.clone();
            stale["profiles"][1]["expected_athlete_id"] = json!(999999);
            let mut unknown = valid.clone();
            unknown["accepted_names"] = json!(["Truman Lefevre"]);
            let mut wrong_url = valid.clone();
            wrong_url["profiles"][1]["source_id"] = json!("999999");
            let mut wrong_canonical = valid.clone();
            wrong_canonical["canonical_athlete_id"] =
                valid["profiles"][1]["expected_athlete_id"].clone();
            let mut unsupported = valid.clone();
            unsupported["reviewed_by"] = json!("inferred_name_match");
            for manifest in [stale, unknown, wrong_url, wrong_canonical, unsupported] {
                let output = identity::apply(test, &manifest).await?;
                assert!(!output.status.success());
                assert!(!output.stderr.is_empty());
                assert_eq!(snapshot(test).await?, before);
            }
            let (path, _) = identity::write_manifest(test, &valid)?;
            let wrong_hash = "0".repeat(64);
            let output = test
                .operation(&crate::support::OfflineOperation::ReviewedIdentity {manifest:std::path::PathBuf::from((path.to_str().context("manifest")?).to_string()),sha256:(&wrong_hash).to_string()})
                .await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("SHA-256 mismatch"));
            assert_eq!(snapshot(test).await?, before);
            sqlx::query("UPDATE athlete_sources SET profile_url='https://wi.milesplit.com/athletes/99999999-another-person' WHERE source_id='17866032'").execute(&test.pool).await?;
            let mismatched_url=snapshot(test).await?;
            let rejected_url=identity::apply(test,&valid).await?;
            assert!(!rejected_url.status.success());
            assert!(String::from_utf8_lossy(&rejected_url.stderr).contains("published profile URL conflicts"));
            assert_eq!(snapshot(test).await?,mismatched_url);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_conflicting_retained_facts_when_user_link_is_applied_then_the_link_fails_atomically()
-> Result<()> {
    for mutation in [
        "gender='female'",
        "state='MI'",
        "graduation_year=2028",
        "school_id=NULL",
    ] {
        support::with_schema(|test|Box::pin(async move {
            identity::seed(test).await?;
            let manifest=identity::manifest(test).await?;
            if mutation=="school_id=NULL" {
                let school: i64=sqlx::query_scalar("INSERT INTO schools(source,source_id,name,state,source_url) VALUES('milesplit','777','Another School','WI','https://wi.milesplit.com/teams/777-another') RETURNING id").fetch_one(&test.pool).await?;
                sqlx::query("WITH changed AS(UPDATE athlete_sources SET school_id=$1 WHERE source_id='17866032' RETURNING athlete_id) UPDATE athletes SET school_id=$1 WHERE id IN(SELECT athlete_id FROM changed)").bind(school).execute(&test.pool).await?;
            } else {
                sqlx::query(sqlx::AssertSqlSafe(format!("WITH changed AS(UPDATE athlete_sources SET {mutation} WHERE source_id='17866032' RETURNING athlete_id) UPDATE athletes SET {mutation} WHERE id IN(SELECT athlete_id FROM changed)"))).execute(&test.pool).await?;
            }
            let before=snapshot(test).await?;
            let output=identity::apply(test,&manifest).await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("conflicting"));
            assert_eq!(snapshot(test).await?,before);
            Ok(())
        })).await?;
    }
    Ok(())
}

#[tokio::test]
async fn given_another_acquisition_session_when_user_link_runs_then_no_rows_change() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            identity::seed(test).await?;
            let manifest = identity::manifest(test).await?;
            let before = snapshot(test).await?;
            let mut connection = test.pool.acquire().await?;
            sqlx::query("SELECT pg_advisory_lock(hashtext(current_schema()),194832741)")
                .execute(&mut *connection)
                .await?;
            let output = identity::apply(test, &manifest).await?;
            sqlx::query("SELECT pg_advisory_unlock(hashtext(current_schema()),194832741)")
                .execute(&mut *connection)
                .await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("another scraper owns"));
            assert_eq!(snapshot(test).await?, before);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_an_approved_pair_when_a_third_profile_claims_same_as_then_it_stays_separate_and_cannot_be_transitively_reviewed()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            identity::seed(test).await?;
            let manifest = identity::manifest(test).await?;
            support::assert_success(&identity::apply(test, &manifest).await?);
            let database = Database::connect(&test.url).await?;
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                "https://wi.milesplit.com/athletes/99999999-third".into(),
                PageContext::default(),
            );
            let person = Athlete {
                id: "99999999".into(),
                name: "Truman Lefevre".into(),
                school_id: Some("14101".into()),
                state: Some("WI".into()),
                gender: Some("male".into()),
                graduation_year: Some(2027),
                profile_url: Some(target.url.clone()),
                same_as: vec!["https://wi.milesplit.com/athletes/14441466".into()],
                ..Default::default()
            };
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".into(),
                body: Vec::new(),
            };
            database
                .persist(
                    None,
                    &target,
                    &capture,
                    &ParsedPage {
                        athletes: vec![person],
                        ..Default::default()
                    },
                )
                .await?;
            assert_eq!(identity::canonical_count(test).await?, 2);
            let third_id: i64 = sqlx::query_scalar(
                "SELECT athlete_id FROM athlete_sources WHERE source_id='99999999'",
            )
            .fetch_one(&test.pool)
            .await?;
            let mut transitive = manifest.clone();
            transitive["profiles"][1] =
                json!({"source":"milesplit","source_id":"99999999","expected_athlete_id":third_id});
            let before = snapshot(test).await?;
            let output = identity::apply(test, &transitive).await?;
            assert!(!output.status.success());
            assert_eq!(snapshot(test).await?, before);
            assert_eq!(
                identity::results(test)
                    .await?
                    .as_array()
                    .context("results")?
                    .len(),
                12
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_a_missing_approval_member_when_replayed_then_null_metadata_cannot_grant_reviewed_name_exemptions()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        identity::seed(test).await?;
        let manifest=identity::manifest(test).await?;
        support::assert_success(&identity::apply(test,&manifest).await?);
        sqlx::query("UPDATE athlete_sources SET details=details-'_reviewed_identity' WHERE source_id='17866032'").execute(&test.pool).await?;
        identity::replay(test,0,"base").await?;
        assert_eq!(identity::canonical_count(test).await?,2);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_an_approved_pair_when_an_unaccepted_name_is_published_then_it_does_not_gain_name_merge_permission()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            identity::seed(test).await?;
            let manifest = identity::manifest(test).await?;
            support::assert_success(&identity::apply(test, &manifest).await?);
            let database = Database::connect(&test.url).await?;
            let body =
                std::fs::read_to_string(support::fixture("identity/truman-14441466-base.html"))?;
            let target = Target::get(
                Source::MileSplit,
                PageKind::Athlete,
                identity::url(0),
                PageContext::default(),
            );
            let mut page = sources::parse(&target, &body)?;
            page.athletes[0].name = "An Unreviewed Different Person".into();
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".into(),
                body: body.into_bytes(),
            };
            database.persist(None, &target, &capture, &page).await?;
            assert_eq!(identity::canonical_count(test).await?, 2);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_a_reviewed_pair_when_source_values_contain_fake_review_metadata_then_the_protected_user_decision_is_unchanged()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        identity::seed(test).await?;
        let manifest=identity::manifest(test).await?;
        support::assert_success(&identity::apply(test,&manifest).await?);
        let before:Value=sqlx::query_scalar("SELECT details->'_reviewed_identity' FROM athlete_sources WHERE source_id='14441466'").fetch_one(&test.pool).await?;
        let database=Database::connect(&test.url).await?;
        let body=std::fs::read_to_string(support::fixture("identity/truman-14441466-base.html"))?;
        let target=Target::get(Source::MileSplit,PageKind::Athlete,identity::url(0),PageContext::default());
        let mut page=sources::parse(&target,&body)?;
        page.athletes[0].source_values.as_mut().context("source values")?.as_object_mut().context("source object")?
            .insert("_reviewed_identity".into(),json!({"preferred_name":"Untrusted Source Name","canonical_athlete_id":999999}));
        let capture=Capture{url:target.url.clone(),status:200,content_type:"text/html".into(),body:body.into_bytes()};
        database.persist(None,&target,&capture,&page).await?;
        let after:Value=sqlx::query_scalar("SELECT details->'_reviewed_identity' FROM athlete_sources WHERE source_id='14441466'").fetch_one(&test.pool).await?;
        assert_eq!(after,before);
        assert_eq!(identity::records(test).await?[0].name,"Truman Lefevre");
        assert_eq!(identity::canonical_count(test).await?,1);
        database.close().await;
        Ok(())
    })).await
}

#[tokio::test]
async fn given_a_sparse_primary_canonical_when_reviewed_then_known_secondary_canonical_fields_are_preserved()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        identity::seed(test).await?;
        let manifest=identity::manifest(test).await?;
        sqlx::query("UPDATE athletes SET state=NULL,gender=NULL,graduation_year=NULL,school_id=NULL WHERE id=(SELECT min(athlete_id) FROM athlete_sources)").execute(&test.pool).await?;
        support::assert_success(&identity::apply(test,&manifest).await?);
        let record=identity::records(test).await?.into_iter().next().context("record")?;
        assert_eq!(record.graduation_year,Some(2027));
        assert_eq!(record.gender.as_deref(),Some("male"));
        assert_eq!(record.schools[0].name,"Appleton East");
        assert_eq!(record.schools[0].state.as_deref(),Some("WI"));
        let state:String=sqlx::query_scalar("SELECT state FROM athletes").fetch_one(&test.pool).await?;
        assert_eq!(state,"WI");
        Ok(())
    })).await
}
