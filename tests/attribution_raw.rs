mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

const RAW: &str = include_str!("../fixtures/milesplit/oh_meet_770621_rs1321880_raw.html");

fn target() -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Results,
        "https://oh.milesplit.com/meets/770621/results/1321880/raw".to_owned(),
        PageContext {
            state: Some("OH".to_owned()),
            sport: Some(Sport::CrossCountry),
            ..PageContext::default()
        },
    )
}

fn linked(id: &str) -> String {
    RAW.replacen(
        "Jeydyn Fields",
        &format!("<a href='/athletes/{id}'>Jeydyn Fields</a>"),
        1,
    )
}

#[test]
fn relay_and_ambiguous_links_remain_unassigned() -> Result<()> {
    let relay = linked("123456").replacen(
        "Boys Middle School 3000 Meter",
        "Boys Middle School 4x400 Meter Relay",
        1,
    );
    let page = sources::parse(&target(), &relay)?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    let first = page.unassigned_results.first().context("relay row")?;
    assert!(first.is_relay);
    assert_eq!(first.mark, "12:40.6");
    assert!(
        first.raw["published_html"]
            .as_str()
            .context("raw HTML")?
            .contains("/athletes/123456")
    );

    let ambiguous = RAW.replacen(
        "Jeydyn Fields",
        "<a href='/athletes/123456'>Jeydyn</a><a href='/athletes/654321'> Fields</a>",
        1,
    );
    let page = sources::parse(&target(), &ambiguous)?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert_eq!(
        page.unassigned_results
            .first()
            .context("ambiguous row")?
            .mark,
        "12:40.6"
    );
    let misplaced = RAW.replacen(
        "Jackson                                    12:40.6",
        "<a href='/athletes/123456'>Jeydyn Fields</a>                              12:40.6",
        1,
    );
    let page = sources::parse(&target(), &misplaced)?;
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    assert_eq!(
        page.unassigned_results
            .first()
            .context("misplaced link row")?
            .mark,
        "12:40.6"
    );
    Ok(())
}

#[tokio::test]
async fn source_id_attribution_upgrades_unassigned_once_and_rejects_conflicts() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let database = Database::connect(&test.url).await?;
        let target = target();
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "text/html".to_owned(), body: Vec::new() };
        let unlinked = sources::parse(&target, RAW)?;
        let linked_page = sources::parse(&target, &linked("123456"))?;
        let conflict = sources::parse(&target, &linked("654321"))?;
        let result_id = unlinked.unassigned_results.first().context("unassigned row")?.result_id.as_deref().context("result ID")?;
        assert_eq!(linked_page.performances.first().context("linked row")?.result_id.as_deref(), Some(result_id));
        database.persist(None, &target, &capture, &unlinked).await?;
        database.persist(None, &target, &capture, &linked_page).await?;
        database.persist(None, &target, &capture, &linked_page).await?;
        database.persist(None, &target, &capture, &unlinked).await?;
        let rows: Vec<(String, String, String, bool)> = sqlx::query_as(
            "SELECT r.source, p.source_id, r.raw_mark, r.raw->'raw' ? 'published_html' FROM results r JOIN athlete_sources p ON p.id = r.athlete_source_id WHERE r.source_result_id = $1"
        ).bind(result_id).fetch_all(&test.pool).await?;
        assert_eq!(rows, vec![("milesplit".to_owned(), "123456".to_owned(), "12:40.6".to_owned(), true)]);
        assert!(database.persist(None, &target, &capture, &conflict).await.is_err());
        let owner: String = sqlx::query_scalar("SELECT p.source_id FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id WHERE r.source_result_id=$1")
            .bind(result_id).fetch_one(&test.pool).await?;
        assert_eq!(owner, "123456");
        database.close().await;
        Ok(())
    })).await
}
