mod support;

use anyhow::{Context, Result};
use support::{assert_success, with_schema};

#[tokio::test]
async fn given_two_and_three_rps_configs_when_native_cli_replays_then_results_remain_identical()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let config = test.directory().join("rates.toml");
        support::write_config(&config)?;
        let original = std::fs::read_to_string(&config)?;
        let input = support::fixture("athleticnet/bio-28872883.json");
        let mut before = None;
        for rate in [2.0,3.0] {
            std::fs::write(&config,original.replace("requests_per_second = 1.0",&format!("requests_per_second = {rate}")))?;
            let output = test.cli(&["scrape","--config",config.to_str().context("UTF-8 config")?,
                "--input",input.to_str().context("UTF-8 fixture")?,"--source","athleticnet",
                "--kind","athlete","--url","https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0",
                "--athlete-id","28872883","--state","WI","--sport","outdoor","--season","2026"]).await?;
            assert_success(&output);
            let records: Vec<(String,String)> = sqlx::query_as("SELECT event,raw_mark FROM results ORDER BY event,raw_mark")
                .fetch_all(&test.pool).await?;
            assert_eq!(records.len(),53);
            assert!(records.contains(&("200 Meters".to_owned(),"23.33a".to_owned())));
            if let Some(previous) = &before { assert_eq!(&records,previous); }
            before = Some(records);
        }
        Ok(())
    })).await
}
