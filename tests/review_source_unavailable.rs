#[path = "support/records.rs"]
mod records;
#[path = "review/support.rs"]
mod review_support;
mod support;

use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result};
use calamine::{Reader, open_workbook_auto};
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    durable::operations::{
        OperationSpec, Workspace, freeze_export, frozen_plan, render_export, verify_export,
    },
    export::{ReviewRow, ReviewStatus},
    model::{PageContext, PageKind, ParsedPage, School, Source, Target},
};

#[derive(Clone, Copy)]
enum Evidence {
    Restricted,
    PublicHeadline,
    PublicResult,
    OtherSection,
    SimilarClass,
}

struct Case {
    id: &'static str,
    year: Option<i32>,
    state: &'static str,
    school_state: &'static str,
    evidence: Evidence,
    unverified_school: bool,
}

async fn seed(database: &Database, case: &Case) -> Result<i64> {
    let mut target = records::target(Source::MileSplit, case.id);
    target.context = PageContext {
        athlete_id: Some(case.id.into()),
        state: Some(case.state.into()),
        ..PageContext::default()
    };
    let mut athlete = records::athlete(case.id);
    athlete.name = format!("Athlete {}", case.id);
    athlete.state = Some(case.state.into());
    athlete.school_id = Some(format!("9{}", case.id));
    athlete.graduation_year = case.year;
    athlete.profile_url = Some(target.url.clone());
    let (section, class) = match case.evidence {
        Evidence::OtherSection => ("article", "progression-cta"),
        Evidence::SimilarClass => ("progression", "not-progression-cta"),
        _ => ("progression", "extra progression-cta source"),
    };
    let pr_rows = if matches!(case.evidence, Evidence::PublicHeadline) {
        vec![
            json!({"masked":false,"html":"<tr data-pr-mode='all' data-pr-level='all'><td class='pr-td-event'>100m</td><td class='pr-td-mark'>11.25</td></tr>"}),
        ]
    } else {
        Vec::new()
    };
    athlete.source_values = Some(
        json!({"pages":{target.url.clone():{"pr_rows":pr_rows,"sections":[],"result_rows":[]},format!("{}/progression",target.url):{"pr_rows":[],"result_rows":[],"sections":[{"attributes":{"id":section,"class":class},"masked":false,"text":"Join MileSplit PRO. Already a PRO member? Log in"}]}}}),
    );
    let performances = if matches!(case.evidence, Evidence::PublicResult) {
        let mut performance = records::performance("11.00");
        performance.athlete_id = case.id.into();
        vec![performance]
    } else {
        Vec::new()
    };
    let page = ParsedPage {
        schools: vec![School {
            id: format!("9{}", case.id),
            name: format!("School {}", case.id),
            state: Some(case.school_state.into()),
            ..School::default()
        }],
        athletes: vec![athlete],
        performances,
        ..ParsedPage::default()
    };
    records::persist(database, &target, &page).await?;
    if case.unverified_school {
        sqlx::query("UPDATE athlete_sources SET details=jsonb_set(details,'{source_values,current_affiliation}',jsonb_build_object('school_state_unverified',true,'unverified_school_state',$2::text)) WHERE source='milesplit' AND source_id=$1")
            .bind(case.id).bind(case.school_state).execute(database.pool()).await?;
    }
    Ok(
        sqlx::query_scalar("SELECT id FROM scrape_pages WHERE url=$1")
            .bind(&target.url)
            .fetch_one(database.pool())
            .await?,
    )
}

async fn database_state(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a),'profiles',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM athlete_sources p),'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),'wire_requests',(SELECT count(*) FROM durable_wire_requests))")
        .fetch_one(database.pool()).await?)
}

fn sheet(path: &Path, name: &str) -> Result<Vec<Vec<String>>> {
    Ok(open_workbook_auto(path)?
        .worksheet_range(name)?
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect())
}

#[tokio::test]
async fn captured_source_unavailability_is_scoped_deduplicated_and_frozen_without_suppressing_public_prs()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let mut expected=Vec::new();
        for (id,year,state,school_state,evidence,unverified_school,diagnostic) in [
            ("11",Some(2027),"WI","WI",Evidence::Restricted,false,true),
            ("12",Some(2027),"WI","WI",Evidence::PublicHeadline,false,false),
            ("13",Some(2027),"WI","WI",Evidence::PublicResult,false,false),
            ("14",Some(2028),"WI","WI",Evidence::Restricted,false,false),
            ("15",Some(2027),"MI","MI",Evidence::Restricted,false,false),
            ("16",Some(2027),"WI","MI",Evidence::Restricted,false,false),
            ("17",Some(2027),"MI","WI",Evidence::Restricted,false,true),
            ("18",None,"WI","WI",Evidence::Restricted,false,false),
            ("19",Some(2027),"WI","MI",Evidence::Restricted,true,false),
            ("20",Some(2027),"WI","WI",Evidence::OtherSection,false,false),
            ("21",Some(2027),"WI","WI",Evidence::SimilarClass,false,false),
            ("22",Some(2027),"WI","",Evidence::Restricted,false,false),
            ("23",Some(2027),"WI","\u{00a0}\t",Evidence::Restricted,false,false),
        ] {
            let checkpoint=seed(&database,&Case {id,year,state,school_state,evidence,unverified_school}).await?;
            if diagnostic {expected.push((checkpoint,id));}
        }
        let alias=Target::get(Source::MileSplit,PageKind::Athlete,"https://wi.milesplit.com/athletes/11-alias".into(),PageContext {athlete_id:Some("11".into()),state:Some("WI".into()),..PageContext::default()});
        database.enqueue(std::slice::from_ref(&alias),false).await?;
        sqlx::query("UPDATE scrape_pages SET status='succeeded',last_success=clock_timestamp() WHERE url=$1").bind(&alias.url).execute(database.pool()).await?;
        let before=database_state(&database).await?;
        let config=review_support::config();
        let mut other=database.begin_export_snapshot(&["MI".into()],&[2027]).await?;
        let other_rows=other.review_records(0,1000).await?;other.finish().await?;
        assert_eq!(other_rows.iter().map(|row|row.athlete_source_id.as_str()).collect::<Vec<_>>(),["15","16"]);
        assert!(other_rows.iter().all(|row|row.state=="MI"));
        let mut next_year=database.begin_export_snapshot(&["WI".into()],&[2028]).await?;
        assert_eq!(next_year.review_records(0,1000).await?.iter().map(|row|row.athlete_source_id.as_str()).collect::<Vec<_>>(),["14"]);next_year.finish().await?;
        let mut snapshot=database.begin_export_snapshot(&config.states,&config.graduation_years).await?;
        let mut actual=Vec::new();
        for _ in 0..4 {
            let cursor=actual.last().map_or(0,|row:&ReviewRow|row.checkpoint_id);
            actual.extend(snapshot.review_records(cursor,1).await?);
        }
        snapshot.finish().await?;
        assert_eq!(actual.iter().map(|row|(row.checkpoint_id,row.athlete_source_id.as_str())).collect::<Vec<_>>(),expected);
        assert!(actual.iter().all(|row|row.status==ReviewStatus::Succeeded && row.state=="WI" && row.cells()[9]=="succeeded" && row.error=="No public PR/result rows in captured profile evidence; source progression is restricted. This does not establish career absence." && row.next_action.contains("Do not retry restricted sections") && row.ai_guidance.contains("do not infer missing results")));
        let workspace=Workspace::open(&test.directory().join("artifacts"),"source-unavailable-review",&OperationSpec::Export {config:config.clone()})?;
        let frozen=freeze_export(&workspace,&config,&test.url).await?;
        assert_eq!(frozen_plan(&workspace,&frozen)?.review_rows,Some(2));
        let workbook=render_export(&workspace,&frozen)?;
        let verified=verify_export(&workspace,&frozen,&workbook)?;
        assert_eq!(verified.review_rows,Some(2));
        let review=sheet(&workbook.path,"Review")?;
        assert_eq!(review[1..],actual.iter().map(ReviewRow::cells).collect::<Vec<_>>());
        let athletes=sheet(&workbook.path,"Athletes")?;
        let names=athletes.iter().skip(1).map(|row|row[0].as_str()).collect::<BTreeSet<_>>();
        assert_eq!(names,BTreeSet::from(["Athlete 11","Athlete 12","Athlete 13","Athlete 17","Athlete 20","Athlete 21"]));
        assert!(athletes.iter().find(|row|row[0]=="Athlete 12").context("public headline athlete")?.iter().any(|cell|cell=="11.25"));
        assert!(athletes.iter().find(|row|row[0]=="Athlete 13").context("public result athlete")?.iter().any(|cell|cell=="11.00 (2026-05-15)"));
        assert_eq!(database_state(&database).await?,before);
        sqlx::query("UPDATE athlete_sources SET details=jsonb_set(details,'{source_values,pages}','{}'::jsonb)").execute(database.pool()).await?;
        let replay=freeze_export(&workspace,&config,&test.url).await?;
        assert_eq!(replay.manifest,frozen.manifest);
        assert_eq!(render_export(&workspace,&replay)?,workbook);
        assert_eq!(sheet(&workbook.path,"Review")?,review);
        let mut fresh=database.begin_export_snapshot(&config.states,&config.graduation_years).await?;
        assert!(fresh.review_records(0,1000).await?.is_empty());fresh.finish().await?;
        database.close().await;
        Ok(())
    })).await
}
