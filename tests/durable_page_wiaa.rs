mod support;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{
    database::Database,
    durable::{
        operations::{OperationSpec, Workspace},
        page::{self, PageOutcome, PageScope, ParsedRef},
    },
    model::{Capture, PageContext, PageKind, ParsedPage, School, Source, Target},
};

fn root_target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://schools.wiaawi.org/Directory/School/List".into(),
        PageContext {
            state: Some("WI".into()),
            season: Some(2026),
            subject_id: Some("wiaa-directory-index".into()),
            ..Default::default()
        },
    )
}

fn root_body() -> String {
    let letters = (b'A'..=b'Z')
        .map(|letter| {
            format!(
                "<option value='{}'>{}</option>",
                char::from(letter),
                char::from(letter)
            )
        })
        .collect::<String>();
    format!(
        "<script>const action='/Directory/School/DirectoryLetter?LetterBtn=';</script><select id='Letter'>{letters}</select>"
    )
}

fn letter_body(letter: char) -> String {
    if letter == 'A' {
        return "<table id='tblSchools'><tbody><tr><td>1</td><td><a class='schoolLink' href='/Directory/School/GetDirectorySchool?orgID=1'><h5>Abbotsford</h5></a></td><td>High School</td><td>Public</td><td>Abbotsford</td></tr></tbody></table>".into();
    }
    format!(
        "<div id='ajaxMsgText'>Nothing was found matching this search criteria</div><input id='SearchLetter' value='{letter}'><input id='SearchType' value='1'>"
    )
}

async fn parsed(
    database: &Database,
    root: &std::path::Path,
    request: &page::PageRequest,
) -> Result<ParsedPage> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT parsed FROM durable_page_receipts WHERE operation_id=$1")
            .bind(&request.binding.operation_id)
            .fetch_one(database.pool())
            .await?;
    let reference: ParsedRef = serde_json::from_value(value)?;
    let bytes = std::fs::read(root.join(format!("{}.blob", reference.body.sha256)))?;
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        reference.body.sha256
    );
    assert_eq!(u64::try_from(bytes.len())?, reference.body.bytes);
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn all_twenty_seven_wiaa_requests_retain_separate_receipts_and_replay_without_new_attempts()
-> Result<()> {
    support::with_schema(|test|Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let root=test.directory().join("wiaa-captures");
        let scope=PageScope {run_generation:"wiaa-directory-test".into(),config_sha256:"a".repeat(64),deadline_ms:chrono::Utc::now().timestamp_millis().checked_add(3_600_000).context("deadline")?,cooldown_ms:15_000};
        let target=root_target();
        let body=root_body();
        assert!(matches!(page::offline_replay(&database,&root,&scope,target.clone(),Capture {url:target.url.clone(),status:200,content_type:"text/html".into(),body:body.as_bytes().to_vec()}).await?,PageOutcome::Completed {..}));
        let request=page::bind_target(&database,target.clone(),&scope).await?;
        let root_page=parsed(&database,&root,&request).await?;
        assert_eq!(root_page.next.len(),26);
        let mut entries=Vec::new();
        for target in root_page.next {
            assert_eq!(target.method.as_deref(),Some("POST"));
            assert!(target.body.is_none());
            let letter=target.url.chars().last().context("letter")?;
            let body=letter_body(letter);
            assert!(matches!(page::offline_replay(&database,&root,&scope,target.clone(),Capture {url:target.url.clone(),status:200,content_type:"text/html".into(),body:body.as_bytes().to_vec()}).await?,PageOutcome::Completed {..}));
            let request=page::bind_target(&database,target.clone(),&scope).await?;
            let parsed=parsed(&database,&root,&request).await?;
            for note in parsed.notes {
                if let Some(rows)=note.strip_prefix("WIAA captured directory entries: ") {
                    entries.extend(serde_json::from_str::<Vec<track_scrapper_v2::sources::wiaa::DirectorySchool>>(rows)?);
                }
            }
            let before:serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p").fetch_one(&test.pool).await?;
            page::offline_replay(&database,&root,&scope,target.clone(),Capture {url:target.url,status:200,content_type:"text/html".into(),body:body.into_bytes()}).await?;
            assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p").fetch_one(&test.pool).await?,before);
        }
        assert_eq!(entries.len(),1);
        assert_eq!(entries[0].name,"Abbotsford");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_page_receipts").fetch_one(&test.pool).await?,27);
        assert_eq!(sqlx::query_as::<_,(i32,i32,i64)>("SELECT min(attempts),max(attempts),count(*) FROM scrape_pages WHERE status='succeeded'").fetch_one(&test.pool).await?,(1,1,27));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM durable_wire_requests").fetch_one(&test.pool).await?,0);
        let provider=Target::get(Source::MileSplit,PageKind::Team,"https://wi.milesplit.com/teams/123".into(),PageContext {state:Some("WI".into()),school_id:Some("123".into()),..Default::default()});
        let school=School {id:"123".into(),name:"Abbotsford".into(),state:Some("WI".into()),city:Some("Abbotsford".into()),..Default::default()};
        database.persist(None,&provider,&Capture {url:provider.url.clone(),status:200,content_type:"text/html".into(),body:Vec::new()},&ParsedPage {schools:vec![school.clone()],..Default::default()}).await?;
        let target=track_scrapper_v2::sources::wiaa::contact_target(Source::MileSplit,&school,&entries[0],2026)?;
        let targets=vec![target];
        let workspace=Workspace::open(&test.directory().join("mapping"),"wiaa-mapping",&OperationSpec::WiaaDirectory {config:track_scrapper_v2::config::Config::default()})?;
        let artifact=workspace.store("wiaa-targets.json",&serde_json::to_vec(&targets)?)?;
        let first=database.durable_enqueue_receipt(&workspace.operation_key()?,&artifact.sha256,&targets,serde_json::json!({"matched":1})).await?;
        assert_eq!(first["enqueued"],1);
        let before:serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p").fetch_one(&test.pool).await?;
        assert_eq!(database.durable_enqueue_receipt(&workspace.operation_key()?,&artifact.sha256,&targets,serde_json::json!({"matched":1})).await?,first);
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p").fetch_one(&test.pool).await?,before);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM scrape_pages WHERE url='https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1' AND cursor->'context'->>'school_id'='123'").fetch_one(&test.pool).await?,1);
        database.close().await;
        Ok(())
    })).await
}
