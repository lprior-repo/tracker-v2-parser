use super::*;

#[tokio::test]
async fn given_valid_website_before_missing_school_when_transaction_fails_then_website_and_queue_roll_back()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let school_id = fixture::insert_school(test, "school-1").await?;
        let (workspace, OperationSpec::ReviewedResearch { manifest }) = build(test, "website-rollback-input", website(school_id), CAPTURE)? else { unreachable!() };
        let mut body: Value = serde_json::from_slice(&workspace.read(&manifest)?)?;
        let mut missing = website(school_id+1); missing["source_id"] = json!("missing-school");
        body["changes"] = json!([website(school_id), missing]);
        let path = test.directory().join("website-rollback.json");
        std::fs::write(&path, serde_json::to_vec(&body)?)?;
        let root = test.directory().join("website-artifacts");
        let spec = OperationSpec::ReviewedResearch { manifest:stage_input(&root, &path)? };
        let workspace = Workspace::open(&root, "website-rollback", &spec)?;
        assert!(fixture::apply(test, &workspace, &spec).await.is_err());
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM schools WHERE website IS NOT NULL OR website_provenance IS NOT NULL").fetch_one(&test.pool).await?, 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrape_pages").fetch_one(&test.pool).await?, 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts").fetch_one(&test.pool).await?, 0);
        Ok(())
    })).await
}

#[tokio::test]
async fn given_malformed_website_provenance_when_review_runs_then_no_existing_audit_is_destroyed()
-> Result<()> {
    with_schema(|test| {
        Box::pin(async move {
            let school_id = fixture::insert_school(test, "school-1").await?;
            for (index, audit) in [
                json!("broken"),
                json!({"manual_research":"broken"}),
                json!({"manual_research_conflicts":"broken"}),
            ]
            .into_iter()
            .enumerate()
            {
                sqlx::query("UPDATE schools SET website_provenance=$2 WHERE id=$1")
                    .bind(school_id)
                    .bind(&audit)
                    .execute(&test.pool)
                    .await?;
                let (workspace, spec) = build(
                    test,
                    &format!("website-malformed-{index}"),
                    website(school_id),
                    CAPTURE,
                )?;
                assert!(fixture::apply(test, &workspace, &spec).await.is_err());
                let retained: Value =
                    sqlx::query_scalar("SELECT website_provenance FROM schools WHERE id=$1")
                        .bind(school_id)
                        .fetch_one(&test.pool)
                        .await?;
                assert_eq!(retained, audit);
            }
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM schools WHERE website IS NOT NULL"
                )
                .fetch_one(&test.pool)
                .await?,
                0
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_citation_link_prefix_without_exact_published_url_when_review_prepares_then_it_rejects_the_claim()
-> Result<()> {
    with_schema(|test| Box::pin(async move {
        let school_id = fixture::insert_school(test, "school-1").await?;
        let (workspace, spec) = build(test, "website-url-prefix", website(school_id), "Test High School https://school.example.edu/evil https://school.example.edu/staff-evil")?;
        assert!(fixture::apply(test, &workspace, &spec).await.is_err());
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM durable_operation_receipts").fetch_one(&test.pool).await?, 0);
        Ok(())
    })).await
}
