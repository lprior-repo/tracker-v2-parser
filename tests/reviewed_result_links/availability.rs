use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::json;
use track_scrapper_v2::export::{ExportLayout, ExportOptions};

use super::{apply, fixture, manifest, support};

#[tokio::test]
async fn given_restricted_profile_when_reviewed_result_is_valid_then_workbook_omits_false_missing_diagnostic()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let fixture = fixture(test).await?;
        sqlx::query("UPDATE scrape_pages SET cursor=cursor||jsonb_build_object('context',COALESCE(cursor->'context','{}'::jsonb)||jsonb_build_object('athlete_id','14474869')) WHERE url=$1")
            .bind(super::PROFILE_URL).execute(fixture.database.pool()).await?;
        sqlx::query("UPDATE athlete_sources SET details=jsonb_set(details,'{source_values}',$2) WHERE id=$1")
            .bind(fixture.athlete_source_id)
            .bind(json!({"pages":{(super::PROFILE_URL):{"pr_rows":[],"result_rows":[],"sections":[{"attributes":{"id":"progression","class":"progression-cta"}}]}}}))
            .execute(fixture.database.pool()).await?;
        let options = ExportOptions { states:vec!["WI".into()],graduation_years:vec![2027],rows_per_file:100,per_athlete_sheets:false };
        let mut before = fixture.database.begin_export_snapshot(&options.states,&options.graduation_years).await?;
        let missing = before.review_records(0,1000).await?;
        assert_eq!(missing.len(),1);
        assert!(missing[0].error.contains("No public PR/result rows"));
        before.finish().await?;
        let (workspace,_,spec) = manifest(&fixture,test)?;
        apply(&fixture,&workspace,&spec).await?;
        let destination = test.directory().join("linked-source-availability.xlsx");
        fixture.database.write_export_with_review(&destination,&options,ExportLayout::SingleSheet,true).await?;
        let mut workbook = open_workbook_auto(&destination)?;
        assert_eq!(workbook.sheet_names(),["Athletes","Review"]);
        assert_eq!(workbook.worksheet_range("Review")?.height(),1);
        let athletes = workbook.worksheet_range("Athletes")?;
        assert_eq!(athletes.height(),2);
        assert!(athletes.rows().nth(1).context("linked athlete row")?.iter().any(|cell|cell.to_string().contains("23:03.4")));
        sqlx::query("UPDATE results SET raw=raw||jsonb_build_object('changed',true) WHERE id=$1")
            .bind(fixture.result_row_id).execute(fixture.database.pool()).await?;
        let mut stale = fixture.database.begin_export_snapshot(&options.states,&options.graduation_years).await?;
        let missing = stale.review_records(0,1000).await?;
        assert_eq!(missing.len(),1);
        assert!(missing[0].error.contains("No public PR/result rows"));
        stale.finish().await?;
        let (records,issues) = fixture.database.export_snapshot(&options.states,&options.graduation_years).await?;
        assert_eq!(records.len(),1);
        assert!(records[0].performances.is_empty());
        assert!(issues.iter().any(|issue|issue.detail.contains("reviewed result link invalidated: result_snapshot_changed")));
        fixture.database.close().await;
        Ok(())
    })).await
}
