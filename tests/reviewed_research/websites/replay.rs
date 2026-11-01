use super::*;

#[tokio::test]
async fn given_reviewed_website_when_crawler_replays_a_conflicting_site_then_reviewed_url_survives_with_unique_conflict_evidence()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            let (workspace, spec) =
                build(test, "website-crawler-replay", website(school_id), CAPTURE)?;
            fixture::apply(test, &workspace, &spec).await?;
            let database = Database::connect(&test.url).await?;
            let target = track_scrapper_v2::model::Target::get(
                track_scrapper_v2::model::Source::AthleticNet,
                track_scrapper_v2::model::PageKind::Team,
                "https://www.athletic.net/team/1".to_owned(),
                Default::default(),
            );
            let page = track_scrapper_v2::model::ParsedPage {
                schools: vec![track_scrapper_v2::model::School {
                    id: "school-1".to_owned(),
                    name: "Different School".to_owned(),
                    city: Some("Different City".to_owned()),
                    state: Some("MN".to_owned()),
                    website: Some("https://unverified.example.edu/".to_owned()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let capture = track_scrapper_v2::model::Capture {
                url: target.url.clone(),
                status: 200,
                content_type: "text/html".to_owned(),
                body: serde_json::to_vec(&page)?,
            };
            database.persist(None, &target, &capture, &page).await?;
            database.persist(None, &target, &capture, &page).await?;
            let row: (String, Value, String, String, String) = sqlx::query_as(
                "SELECT website,website_provenance,name,city,state FROM schools WHERE id=$1",
            )
            .bind(school_id)
            .fetch_one(&test.pool)
            .await?;
            assert_eq!(row.0, "https://school.example.edu/");
            assert_eq!(row.1["manual_research"].as_array().unwrap().len(), 1);
            let conflicts = row.1["manual_research_conflicts"].as_array().unwrap();
            assert_eq!(conflicts.len(), 4);
            assert_eq!(
                (row.2.as_str(), row.3.as_str(), row.4.as_str()),
                ("Test High School", "Madison", "WI")
            );
            assert!(conflicts.iter().any(|conflict| conflict["field"] == "name"
                && conflict["incoming_value"] == "Different School"));
            assert!(
                conflicts
                    .iter()
                    .any(|conflict| conflict["field"] == "website"
                        && conflict["incoming_value"] == "https://unverified.example.edu/")
            );
            assert_eq!(conflicts[0]["needs_review"], true);
            let (_, issues) = database
                .export_snapshot(&["WI".to_owned()], &[2027])
                .await?;
            let review = issues
                .iter()
                .filter(|issue| {
                    issue
                        .detail
                        .starts_with("manual research school conflict: ")
                })
                .collect::<Vec<_>>();
            assert_eq!(review.len(), 1);
            assert!(review[0].detail.contains("https://unverified.example.edu/"));
            assert!(review[0].detail.contains("website"));
            database.close().await;
            Ok(())
        })
    })
    .await
}
