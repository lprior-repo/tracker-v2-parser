mod support;

use anyhow::{Context, Result};
use support::{assert_success, with_schema};

#[tokio::test]
async fn given_two_and_three_rps_configs_when_offline_adapter_replays_then_results_remain_identical()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let config = test.directory().join("rates.toml");
        support::write_config(&config)?;
        let original = std::fs::read_to_string(&config)?;
        let input = support::fixture("athleticnet/bio-28872883.json");
        let mut before = None;
        for rate in [2.0,3.0] {
            std::fs::write(&config,original.replace("requests_per_second = 1.0",&format!("requests_per_second = {rate}\ncontact_requests_per_second = {rate}"))
                .replace("concurrency = 1", "concurrency = 3"))?;
            let output = test.operation(&crate::support::OfflineOperation::Replay {config:Some(std::path::PathBuf::from((config.to_str().context("UTF-8 config")?).to_string())),input:std::path::PathBuf::from((input.to_str().context("UTF-8 fixture")?).to_string()),target:track_scrapper_v2::model::Target::get(track_scrapper_v2::model::Source::AthleticNet,track_scrapper_v2::model::PageKind::Athlete,("https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0").to_string(),track_scrapper_v2::model::PageContext {state:Some(("WI").to_string()),athlete_id:Some(("28872883").to_string()),season:Some(("2026").parse::<i32>()?),sport:Some(track_scrapper_v2::model::Sport::Outdoor), ..Default::default()})}).await?;
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
