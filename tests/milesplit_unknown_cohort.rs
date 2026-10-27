mod support;

use anyhow::Result;

#[tokio::test]
async fn source_unknown_cohorts_survive_roster_replay_without_losing_known_years() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let fixture = support::fixture("milesplit/roster-al-addison.html");
        let path = fixture.to_string_lossy();
        let args = ["scrape", "--input", &path, "--source", "milesplit", "--kind", "roster",
            "--url", "https://al.milesplit.com/teams/6253-addison/roster", "--state", "AL"];
        for _ in 0..2 {
            let output = test.cli(&args).await?;
            support::assert_success(&output);
        }
        let rows: Vec<(String, String, Option<i32>)> = sqlx::query_as(
            "SELECT source_id,name,graduation_year FROM athlete_sources WHERE source='milesplit' AND source_id IN ('15957867','15957874','15882164') ORDER BY source_id")
            .fetch_all(&test.pool).await?;
        assert_eq!(rows, vec![
            ("15882164".into(), "Brody Daniel".into(), None),
            ("15957867".into(), "Bady Daniel".into(), None),
            ("15957874".into(), "Brady Daniel".into(), Some(2030)),
        ]);
        let checkpoint: (String, i64) = sqlx::query_as(
            "SELECT min(status),count(*) FROM scrape_pages WHERE url='https://al.milesplit.com/teams/6253-addison/roster'")
            .fetch_one(&test.pool).await?;
        assert_eq!(checkpoint, ("succeeded".into(), 1));
        Ok(())
    })).await
}
