mod support;

use anyhow::Result;
use track_scrapper_v2::{
    database::Database,
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    sources,
};

#[tokio::test]
async fn linked_raw_schools_are_persisted_before_identified_and_unassigned_results() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let target = Target::get(
            Source::MileSplit,
            PageKind::Results,
            "https://oh.milesplit.com/meets/770621/results/1321880/raw".to_owned(),
            PageContext { sport: Some(Sport::CrossCountry), ..PageContext::default() },
        );
        let fixture = std::fs::read_to_string(support::fixture("milesplit/oh_meet_770621_rs1321880_raw.html"))?;
        let body = fixture
            .replacen("Jeydyn Fields", "<a href='/athletes/123456'>Jeydyn Fields</a>", 1)
            .replacen("8 Jackson                                    12:40.6", "8 <a href='/teams/98765-jackson'>Jackson</a>                                    12:40.6", 1)
            .replacen("8 Oak Hill                                   12:54.8", "8 <a href='/teams/98766-oak-hill'>Oak Hill</a>                                   12:54.8", 1);
        let page = sources::parse(&target, &body)?;
        assert_eq!(page.schools.len(), 2);
        let capture = Capture { url: target.url.clone(), status: 200, content_type: "text/html".to_owned(), body: body.as_bytes().to_vec() };
        let database = Database::connect(&test.url).await?;
        database.persist(None, &target, &capture, &page).await?;
        database.persist(None, &target, &capture, &page).await?;
        let schools: Vec<(String, String)> = sqlx::query_as("SELECT source_id, name FROM schools ORDER BY source_id")
            .fetch_all(&test.pool).await?;
        assert_eq!(schools, vec![("98765".to_owned(), "Jackson".to_owned()), ("98766".to_owned(), "Oak Hill".to_owned())]);
        let results: Vec<(String, String, bool)> = sqlx::query_as("SELECT s.source_id, r.raw_mark, r.athlete_id IS NOT NULL FROM results r JOIN schools s ON s.id = r.represented_school_id ORDER BY s.source_id")
            .fetch_all(&test.pool).await?;
        assert_eq!(results, vec![("98765".to_owned(), "12:40.6".to_owned(), true), ("98766".to_owned(), "12:54.8".to_owned(), false)]);
        let athlete_school: String = sqlx::query_scalar("SELECT s.source_id FROM athlete_sources a JOIN schools s ON s.id = a.school_id WHERE a.source_id = '123456'")
            .fetch_one(&test.pool).await?;
        assert_eq!(athlete_school, "98765");
        database.close().await;
        Ok(())
    })).await
}
