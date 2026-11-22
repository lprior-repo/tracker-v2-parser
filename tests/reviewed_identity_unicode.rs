#[path = "support/identity_review.rs"]
mod identity;
mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{self, OperationSpec, Workspace},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Target},
    sources,
};

const NAMES: [&str; 2] = ["İpek Çelik", "Ipek Celik"];

struct Observation {
    target: Target,
    capture: Capture,
    parsed: ParsedPage,
}

impl Observation {
    fn new(index: usize, name: &str) -> Result<Self> {
        let original = ["Truman LeFerve", "Truman Lefevre"][index];
        let html = std::fs::read_to_string(support::fixture(&format!(
            "identity/truman-{}-base.html",
            identity::PROFILES[index].0
        )))?
        .replace(original, name)
        .replace("Class of 2027", "Class of 2028");
        let target = Target::get(
            Source::MileSplit,
            PageKind::Athlete,
            identity::url(index),
            PageContext {
                athlete_id: Some(identity::PROFILES[index].0.into()),
                state: Some("WI".into()),
                season: Some(2026),
                ..PageContext::default()
            },
        );
        let parsed = sources::parse(&target, &html)?;
        assert_eq!(parsed.athletes.len(), 1);
        assert_eq!(parsed.athletes[0].name, name);
        assert_eq!(parsed.athletes[0].graduation_year, Some(2028));
        assert_eq!(parsed.performances.len(), 6);
        let capture = Capture {
            url: target.url.clone(),
            status: 200,
            content_type: "text/html".into(),
            body: html.into_bytes(),
        };
        Ok(Self {
            target,
            capture,
            parsed,
        })
    }

    async fn persist(&self, database: &Database) -> Result<()> {
        database
            .persist(None, &self.target, &self.capture, &self.parsed)
            .await?;
        Ok(())
    }
}

struct Decision {
    workspace: Workspace,
    spec: OperationSpec,
    report: Value,
    canonical: i64,
    audit: Value,
}

async fn audit(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_agg(jsonb_build_object('id',id,'review',details->'_reviewed_identity') ORDER BY id) \
         FROM athlete_sources",
    )
    .fetch_one(database.pool())
    .await?)
}

async fn state(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object( \
         'athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a), \
         'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p), \
         'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r), \
         'pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p), \
         'receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_key) FROM durable_operation_receipts r))",
    )
    .fetch_one(database.pool())
    .await?)
}

async fn reviewed_pair(test: &support::Scenario) -> Result<(Database, [Observation; 2], Decision)> {
    let database = Database::connect(&test.url).await?;
    let observations = [
        Observation::new(0, NAMES[0])?,
        Observation::new(1, NAMES[1])?,
    ];
    for observation in &observations {
        observation.persist(&database).await?;
    }
    assert_eq!(identity::canonical_count(test).await?, 2);
    let mut manifest = identity::manifest(test).await?;
    manifest["preferred_name"] = json!(NAMES[0]);
    manifest["accepted_names"] = json!(NAMES);
    manifest["reason"] = json!("Fixture review accepts these two published Unicode name variants.");
    let canonical = manifest["canonical_athlete_id"]
        .as_i64()
        .context("canonical ID")?;
    let (path, digest) = identity::write_manifest(test, &manifest)?;
    let root = test.directory().join("unicode-identity-review");
    let artifact = operations::stage_input(&root, &path)?;
    assert_eq!(artifact.sha256, digest);
    let spec = OperationSpec::ReviewedIdentity { manifest: artifact };
    let workspace = Workspace::open(&root, "unicode-identity-review", &spec)?;
    let report = operations::execute_import(&workspace, &spec, &test.url).await?;
    assert_eq!(report["profile_count"], 2);
    assert_eq!(report["result_count"], 12);
    assert_eq!(report["already_applied"], false);
    assert_eq!(identity::canonical_count(test).await?, 1);
    let audit = audit(&database).await?;
    assert_eq!(
        audit[0]["review"]["accepted_names"],
        json!(["i\u{307}pek çelik", "ipek celik"])
    );
    Ok((
        database,
        observations,
        Decision {
            workspace,
            spec,
            report,
            canonical,
            audit,
        },
    ))
}

async fn assert_ownership(database: &Database, expected: usize) -> Result<()> {
    let owners: (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*),count(DISTINCT r.athlete_id), \
         count(*) FILTER (WHERE r.athlete_id<>p.athlete_id) \
         FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(owners, (12, i64::try_from(expected)?, 0));
    let (records, _) = database.export_snapshot(&["WI".into()], &[2028]).await?;
    assert_eq!(records.len(), expected);
    assert_eq!(
        records
            .iter()
            .map(|record| record.performances.len())
            .sum::<usize>(),
        12
    );
    Ok(())
}

async fn assert_receipt_replay(
    test: &support::Scenario,
    database: &Database,
    decision: &Decision,
) -> Result<()> {
    let before = state(database).await?;
    assert_eq!(
        operations::execute_import(&decision.workspace, &decision.spec, &test.url).await?,
        decision.report
    );
    assert_eq!(state(database).await?, before);
    Ok(())
}

#[tokio::test]
async fn given_reviewed_unicode_aliases_when_unchanged_profiles_are_replayed_then_one_person_and_all_results_remain_linked()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let (database, observations, decision) = reviewed_pair(test).await?;
            let results = identity::results(test).await?;
            for _ in 0..2 {
                for observation in &observations {
                    observation.persist(&database).await?;
                    assert_eq!(
                        identity::canonical_count(test).await?,
                        1,
                        "unchanged approved name split the reviewed person"
                    );
                }
            }
            assert_eq!(audit(&database).await?, decision.audit);
            assert_eq!(identity::results(test).await?, results);
            assert_ownership(&database, 1).await?;
            let records = database.athlete_records().await?;
            assert_eq!(records[0].id, decision.canonical);
            assert_eq!(records[0].name, NAMES[0]);
            assert_eq!(records[0].profiles.len(), 2);
            assert_receipt_replay(test, &database, &decision).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_reviewed_unicode_aliases_when_an_unapproved_name_is_observed_then_the_profile_splits_and_review_audit_survives()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let (database, _, decision) = reviewed_pair(test).await?;
            Observation::new(0, "Unapproved Runner")?
                .persist(&database)
                .await?;
            assert_eq!(identity::canonical_count(test).await?, 2);
            assert_eq!(audit(&database).await?, decision.audit);
            let owners: Vec<(String, i64, String, bool)> = sqlx::query_as(
                "SELECT source_id,athlete_id,details->>'name', \
             COALESCE((details->>'_identity_conflict')::boolean,FALSE) \
             FROM athlete_sources ORDER BY source_id",
            )
            .fetch_all(database.pool())
            .await?;
            assert_eq!(owners.len(), 2);
            assert_eq!(owners[0].0, identity::PROFILES[0].0);
            assert_ne!(owners[0].1, decision.canonical);
            assert_eq!(owners[0].2, "Unapproved Runner");
            assert!(owners[0].3);
            assert_eq!(owners[1].0, identity::PROFILES[1].0);
            assert_eq!(owners[1].1, decision.canonical);
            assert_ownership(&database, 2).await?;
            assert_receipt_replay(test, &database, &decision).await?;
            database.close().await;
            Ok(())
        })
    })
    .await
}
