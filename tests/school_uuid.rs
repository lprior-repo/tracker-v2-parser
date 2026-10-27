mod support;

use anyhow::Result;
use calamine::{Reader, open_workbook_auto};
use track_scrapper_v2::{
    database::Database,
    export::{ExportOptions, write_spreadsheets},
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Target},
};

#[tokio::test]
async fn existing_schools_receive_distinct_uuids_without_changing_foreign_keys() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        sqlx::raw_sql(include_str!("../migrations/202610260001_scraper.sql"))
            .execute(&test.pool).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,source_url) VALUES ('athleticnet','1','First','https://example.com/1'),('milesplit','2','Second','https://example.com/2')")
            .execute(&test.pool).await?;
        sqlx::query("INSERT INTO athletes(name,school_id) SELECT 'Runner',id FROM schools WHERE source_id='1'")
            .execute(&test.pool).await?;
        let before: i64 = sqlx::query_scalar("SELECT school_id FROM athletes WHERE name='Runner'")
            .fetch_one(&test.pool).await?;

        sqlx::raw_sql(include_str!("../migrations/202610270002_school_uuid.sql"))
            .execute(&test.pool).await?;

        let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id,uuid::text FROM schools ORDER BY id")
            .fetch_all(&test.pool).await?;
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].1, rows[1].1);
        assert_eq!(rows[0].1.len(), 36);
        let linked: (i64, String) = sqlx::query_as("SELECT a.school_id,s.uuid::text FROM athletes a JOIN schools s ON s.id=a.school_id WHERE a.name='Runner'")
            .fetch_one(&test.pool).await?;
        assert_eq!(linked, (before, rows[0].1.clone()));
        let duplicate = sqlx::query("INSERT INTO schools(source,source_id,name,source_url,uuid) SELECT 'athleticnet','3','Third','https://example.com/3',uuid FROM schools WHERE source_id='1'")
            .execute(&test.pool).await;
        assert!(duplicate.is_err());
        let null = sqlx::query("INSERT INTO schools(source,source_id,name,source_url,uuid) VALUES ('athleticnet','4','Fourth','https://example.com/4',NULL)")
            .execute(&test.pool).await;
        assert!(null.is_err());
        Ok(())
    })).await
}

#[tokio::test]
async fn persisted_school_keeps_uuid_across_replays_and_reconnections() -> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = Target::get(
                Source::AthleticNet,
                PageKind::Team,
                "https://www.athletic.net/team/1".to_owned(),
                PageContext::default(),
            );
            let capture = Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "application/json".to_owned(),
                body: Vec::new(),
            };
            let page = ParsedPage {
                schools: vec![
                    School {
                        id: "1".to_owned(),
                        name: "First".to_owned(),
                        ..School::default()
                    },
                    School {
                        id: "2".to_owned(),
                        name: "Second".to_owned(),
                        ..School::default()
                    },
                ],
                ..ParsedPage::default()
            };
            database.persist(None, &target, &capture, &page).await?;
            let first: Vec<(String, String)> =
                sqlx::query_as("SELECT source_id,uuid::text FROM schools ORDER BY source_id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(first.len(), 2);
            assert_ne!(first[0].1, first[1].1);
            database.persist(None, &target, &capture, &page).await?;
            database.close().await;
            let database = Database::connect(&test.url).await?;
            database.persist(None, &target, &capture, &page).await?;
            let replayed: Vec<(String, String)> =
                sqlx::query_as("SELECT source_id,uuid::text FROM schools ORDER BY source_id")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(first, replayed);
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn exported_school_uuid_matches_the_persisted_identity() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        sqlx::query("INSERT INTO schools(source,source_id,name,state,source_url) VALUES ('athleticnet','1','First','WI','https://example.com/1')")
            .execute(&test.pool).await?;
        sqlx::query("INSERT INTO athletes(name,state,graduation_year) VALUES ('Runner','WI',2028)")
            .execute(&test.pool).await?;
        sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,school_id) SELECT a.id,'athleticnet','runner',s.id FROM athletes a CROSS JOIN schools s WHERE a.name='Runner' AND s.source_id='1'")
            .execute(&test.pool).await?;
        let uuid: String = sqlx::query_scalar("SELECT uuid::text FROM schools WHERE source_id='1'")
            .fetch_one(&test.pool).await?;
        let records = database.athlete_records().await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schools[0].uuid.as_deref(), Some(uuid.as_str()));
        let options = ExportOptions { states: vec!["WI".to_owned()], graduation_years: vec![2028],
            rows_per_file: 100, per_athlete_sheets: false };
        let path = test.directory().join("schools.xlsx");
        write_spreadsheets(&path, &records, &options)?;
        let mut workbook = open_workbook_auto(&path)?;
        let sheet = workbook.worksheet_range("Athletes")?;
        let mut rows = sheet.rows();
        let headers = rows.next().ok_or_else(|| anyhow::anyhow!("missing headers"))?;
        let values = rows.next().ok_or_else(|| anyhow::anyhow!("missing athlete"))?;
        let index = headers.iter().position(|cell| cell.to_string() == "school_uuids")
            .ok_or_else(|| anyhow::anyhow!("missing school UUID header"))?;
        assert_eq!(values.get(index).map(ToString::to_string), Some(uuid));
        database.close().await;
        Ok(())
    })).await
}
