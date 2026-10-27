mod support;

use anyhow::Result;
use track_scrapper_v2::database::Database;

const ROSTER_URL: &str = "https://ca.milesplit.com/teams/990001-synthetic-school/roster";
const UNNAMED: &str = r#"<li class="athlete-row"><a href="/athletes/990011-not-a-published-name"><img alt="Not a published athlete name"></a><span class="column-grad-year">-</span><span class="column-gender">F</span><span data-season-id="3"><svg class="icon-yes"></svg></span></li>"#;
const NAMED: &str = r#"<li class="athlete-row"><a href="/athletes/990012-not-the-name">Example, Avery</a><span class="column-grad-year">2027</span><span class="column-gender">F</span><span data-season-id="2"><svg class="icon-yes"></svg></span></li>"#;

async fn import_synthetic(test: &support::Scenario, rows: &str) -> Result<std::process::Output> {
    let input = test.directory().join("explicitly-synthetic-roster.html");
    let html = format!(
        r#"<!doctype html><html><head><title>Synthetic roster test only</title><link rel="canonical" href="{ROSTER_URL}"></head><body><div class="jumbotron-content"><h1>Synthetic School</h1></div><ul id="rosterDataset">{rows}</ul></body></html>"#
    );
    tokio::fs::write(&input, html).await?;
    test.cli(&[
        "scrape",
        "--input",
        &input.to_string_lossy(),
        "--source",
        "milesplit",
        "--kind",
        "roster",
        "--url",
        ROSTER_URL,
        "--state",
        "CA",
        "--season",
        "2026",
    ])
    .await
}

#[tokio::test]
async fn given_synthetic_partial_roster_when_cli_replays_then_named_identity_and_followups_commit()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let rows = format!("{UNNAMED}{NAMED}");
        for _ in 0..2 {
            support::assert_success(&import_synthetic(test, &rows).await?);
        }
        let athletes: Vec<(String, String, Option<i32>, Option<String>)> = sqlx::query_as(
            "SELECT source_id,name,graduation_year,gender FROM athlete_sources ORDER BY source_id")
            .fetch_all(&test.pool).await?;
        assert_eq!(athletes, vec![("990012".into(), "Avery Example".into(), Some(2027), Some("female".into()))]);
        let canonical: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
            .fetch_one(&test.pool).await?;
        assert_eq!(canonical, 1);
        let followups: Vec<(String, String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT cursor->'context'->>'athlete_id',cursor->'context'->>'school_id',cursor->'context'->>'school_name',cursor->'context'->>'state',cursor->'context'->>'sport',cursor->'context'->>'season',status FROM scrape_pages WHERE cursor->>'kind'='athlete' ORDER BY cursor->'context'->>'athlete_id'")
            .fetch_all(&test.pool).await?;
        assert_eq!(followups, vec![
            ("990011".into(), "990001".into(), "Synthetic School".into(), "CA".into(), "cross_country".into(), "2026".into(), "pending".into()),
            ("990012".into(), "990001".into(), "Synthetic School".into(), "CA".into(), "outdoor".into(), "2026".into(), "pending".into()),
        ]);
        let checkpoints: Vec<(String, Vec<String>)> = sqlx::query_as(
            "SELECT status,notes FROM scrape_pages WHERE url=$1")
            .bind(ROSTER_URL).fetch_all(&test.pool).await?;
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].0, "succeeded");
        assert_eq!(checkpoints[0].1.len(), 1);
        let observation = checkpoints[0].1[0].split_once("; ").expect("observation JSON").1;
        let observation: serde_json::Value = serde_json::from_str(observation)?;
        assert_eq!(observation["athlete_id"], "990011");
        assert_eq!(observation["school_id"], "990001");
        assert_eq!(observation["graduation_claim"], "-");
        assert_eq!(observation["graduation_year"], serde_json::Value::Null);
        assert_eq!(observation["gender"], "female");
        assert_eq!(observation["sports"], serde_json::json!(["cross_country"]));
        assert!(observation["row"].as_str().expect("public row").contains("Not a published athlete name"));
        Ok(())
    })).await
}

#[tokio::test]
async fn given_synthetic_unnamed_only_roster_when_imported_then_issues_say_incomplete_not_empty()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            support::assert_success(&import_synthetic(test, UNNAMED).await?);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(&test.pool)
                .await?;
            assert_eq!(count, 0);
            let database = Database::connect(&test.url).await?;
            let (_, issues) = database.export_snapshot(&["CA".into()], &[2027]).await?;
            let details: Vec<_> = issues
                .iter()
                .filter(|issue| issue.detail.contains("roster"))
                .collect();
            assert!(details.iter().any(|issue| {
                issue
                    .detail
                    .contains("Incomplete MileSplit roster observation")
            }));
            assert!(
                !issues
                    .iter()
                    .any(|issue| issue.detail.contains("roster is empty"))
            );
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_synthetic_avatar_then_same_id_text_when_imported_then_published_name_and_unknown_year_survive()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let row = r#"<li class="athlete-row"><a href="/athletes/990013-guess-nothing"><img alt="Wrong Name"></a><a href="/athletes/990013-guess-nothing">Example, Jordan</a><span class="column-grad-year">-</span><span class="column-gender">M</span><span data-season-id="1"><svg class="icon-yes"></svg></span></li>"#;
        support::assert_success(&import_synthetic(test, row).await?);
        let athlete: (String, String, Option<i32>, String) = sqlx::query_as(
            "SELECT source_id,name,graduation_year,gender FROM athlete_sources")
            .fetch_one(&test.pool).await?;
        assert_eq!(athlete, ("990013".into(), "Jordan Example".into(), None, "male".into()));
        let followup: String = sqlx::query_scalar(
            "SELECT cursor->'context'->>'sport' FROM scrape_pages WHERE cursor->>'kind'='athlete'")
            .fetch_one(&test.pool).await?;
        assert_eq!(followup, "indoor");
        let notes: Vec<String> = sqlx::query_scalar("SELECT notes FROM scrape_pages WHERE url=$1")
            .bind(ROSTER_URL).fetch_one(&test.pool).await?;
        assert!(notes.is_empty());
        Ok(())
    })).await
}

#[tokio::test]
async fn given_synthetic_invalid_identity_when_imported_then_failure_remains_explicit_and_atomic()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        for (row, error) in [
            ("<li class='athlete-row'>No profile</li>", "missing athlete profile identity"),
            ("<li class='athlete-row'><a href='/athletes/not-an-id'>Name</a></li>", "invalid positive MileSplit ID"),
            ("<li class='athlete-row'><a href='/athletes/990014'></a><a href='/athletes/990015'>Different identity</a></li>", "conflicting athlete identities"),
        ] {
            let output = import_synthetic(test, &format!("{NAMED}{row}")).await?;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains(error));
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM athletes")
                .fetch_one(&test.pool).await?;
            assert_eq!(count, 0);
            let followups: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE cursor->>'kind'='athlete'")
                .fetch_one(&test.pool).await?;
            assert_eq!(followups, 0);
        }
        Ok(())
    })).await
}
