mod support;

use anyhow::Result;
use serde_json::{Value, json};
use track_scrapper_v2::{
    database::Database,
    model::{Athlete, Capture, PageContext, PageKind, ParsedPage, Performance, Source, Target},
};

fn performances() -> Result<Vec<Performance>> {
    (0..129)
        .map(|index| {
            Ok(serde_json::from_value(json!({
                "athlete_id":"123","event":"100m","mark":"11.20","sport":"outdoor",
                "result_id":format!("public-batch-{index}"),"school_id":null,"meet_id":"proof-meet",
                "meet_name":"Proof","date":"2026-05-01","wind":0.0,"timing":"FAT","course":null,
                "is_personal_record":true,"is_relay":false,"season":2026,"grade":"11","gender":"M",
                "source_values":{"context":{"season":2026},"row":{"attributes":{"place":"1"}}}
            }))?)
        })
        .collect()
}

fn target() -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Athlete,
        "https://www.athletic.net/athlete/123".into(),
        PageContext::default(),
    )
}

fn capture(target: &Target) -> Capture {
    Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".into(),
        body: Vec::new(),
    }
}

fn page(rows: Vec<Performance>) -> ParsedPage {
    ParsedPage {
        athletes: vec![Athlete {
            id: "123".into(),
            name: "First athlete".into(),
            ..Athlete::default()
        }],
        performances: rows,
        ..ParsedPage::default()
    }
}

async fn state(split: bool) -> Result<Vec<Value>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    support::with_schema(move |test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let target = target();
            let capture = capture(&target);
            let initial = performances()?;
            for correction in [false, true] {
                let mut rows = initial.clone();
                if correction {
                    for row in &mut rows {
                        row.mark = "10.90".into();
                        row.source_values = Some(json!({"row":{"attributes":{"place":"2"}}}));
                    }
                }
                if split {
                    for row in rows {
                        database
                            .persist(None, &target, &capture, &page(vec![row]))
                            .await?;
                    }
                } else {
                    database
                        .persist(None, &target, &capture, &page(rows))
                        .await?;
                }
            }
            let output: Vec<Value> = sqlx::query_scalar(
                "SELECT to_jsonb(r)-'id'-'last_seen' FROM results r ORDER BY source_result_key",
            )
            .fetch_all(&test.pool)
            .await?;
            let checkpoint: (i64, String) =
                sqlx::query_as("SELECT count(*),min(status) FROM scrape_pages")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(checkpoint, (1, "succeeded".into()));
            database.close().await;
            sender
                .send(output)
                .map_err(|_| anyhow::anyhow!("comparison receiver closed"))?;
            Ok(())
        })
    })
    .await?;
    Ok(receiver.await?)
}

#[tokio::test]
async fn given_129_results_when_page_batched_and_replayed_then_public_persistence_matches_single_rows()
-> Result<()> {
    let serial = state(true).await?;
    let batched = state(false).await?;
    assert_eq!(serial.len(), 129);
    assert_eq!(serial, batched);
    assert!(batched.iter().all(|row| row["raw_mark"] == "10.90"));
    Ok(())
}

#[tokio::test]
async fn given_result_identity_conflict_when_public_page_fails_then_all_page_effects_roll_back()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database=Database::connect(&test.url).await?;
        let target=target();let capture=capture(&target);let initial=page(performances()?);
        database.persist(None,&target,&capture,&initial).await?;
        let before:Value=sqlx::query_scalar("SELECT jsonb_build_object('results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),'athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a),'profiles',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM athlete_sources s),'pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p))").fetch_one(&test.pool).await?;
        let mut incoming=initial.clone();
        incoming.athletes.push(Athlete {id:"456".into(),name:"Different athlete".into(),..Athlete::default()});
        incoming.performances[17].athlete_id="456".into();
        incoming.performances[0].mark="9.90".into();
        incoming.notes.push("must roll back".into());
        let error=database.persist(None,&target,&capture,&incoming).await.expect_err("identity collision must reject whole page");
        assert!(error.to_string().contains("different athlete"),"{error:#}");
        let after:Value=sqlx::query_scalar("SELECT jsonb_build_object('results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM results r),'athletes',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM athletes a),'profiles',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM athlete_sources s),'pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p))").fetch_one(&test.pool).await?;
        assert_eq!(before,after);
        database.close().await;Ok(())
    })).await
}
