mod support;

use anyhow::Result;
use sqlx::AssertSqlSafe;
use track_scrapper_v2::database::Database;

#[tokio::test]
async fn given_mixed_duplicate_and_unverified_affiliations_when_scoping_then_export_ids_match_full_records()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            sqlx::query(
                "INSERT INTO schools(source,source_id,name,state,source_url) SELECT 'milesplit',number::text,'School ' || number,state,'https://example.org/' || number FROM unnest(ARRAY[NULL,'MN','WI',U&'\\00A0mN\\2003',U&'\\2003\\00A0','']) WITH ORDINALITY s(state,number)",
            )
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO athletes(name,state,graduation_year,gender) SELECT 'Scope ' || number, (ARRAY[NULL,'MN','WI',U&'\\00A0mN\\2003',U&'\\2003\\00A0',''])[1+number%6], (ARRAY[2027,2028,2029,NULL])[1+(number/6)%4], (ARRAY['male','female',NULL])[1+(number/24)%3] FROM generate_series(0,575) number",
            )
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO athlete_sources(athlete_id,source,source_id,school_id,profile_url,details) SELECT a.id,'milesplit',a.id::text,s.id,'https://mn.milesplit.com/athletes/' || a.id,jsonb_build_object('source_values',jsonb_build_object('current_affiliation',jsonb_build_object('school_state_unverified',(a.id/36)%2=0,'unverified_school_state',CASE WHEN (a.id/72)%2=0 THEN s.state ELSE 'WI' END),'retained_page',repeat('source',2000))) FROM athletes a JOIN schools s ON s.source_id=(1+(a.id/6)%6)::text",
            )
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO athlete_sources(athlete_id,source,source_id,school_id,profile_url,details) SELECT athlete_id,'athleticnet',source_id,school_id,'https://www.athletic.net/athlete/' || athlete_id,CASE WHEN athlete_id%3=0 THEN '{}'::jsonb ELSE details END FROM athlete_sources WHERE source='milesplit'",
            )
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO athlete_sources(athlete_id,source,source_id,school_id,profile_url) SELECT a.id,'milesplit','secondary-' || a.id,s.id,'https://mn.milesplit.com/athletes/secondary-' || a.id FROM athletes a JOIN schools s ON s.source_id=CASE WHEN a.id%2=0 THEN '2' ELSE '3' END WHERE a.id%4=0",
            )
            .execute(database.pool())
            .await?;
            let scope_sql = include_str!("../src/database/export_snapshot_scope.sql")
                .replace("LIMIT 1", "LIMIT 10000");
            for states in [
                vec!["MN".to_owned()],
                vec!["WI".to_owned()],
                vec!["\u{00a0}mN\u{2003}".to_owned()],
                vec!["MN".to_owned(), "WI".to_owned()],
                Vec::new(),
            ] {
                for years in [vec![2027], vec![2028, 2029], Vec::new()] {
                    let records = database.athlete_records_for(&states, &years).await?;
                    let expected = records.iter().map(|record| record.id).collect::<Vec<_>>();
                    assert!(!expected.is_empty());
                    for cursor in [0, 149, 399, 576] {
                        let actual: Vec<i64> = sqlx::query_scalar(AssertSqlSafe(scope_sql.as_str()))
                            .bind(&states)
                            .bind(&years)
                            .bind(i64::from(cursor))
                            .fetch_all(database.pool())
                            .await?;
                        assert_eq!(
                            actual,
                            expected.iter().copied().filter(|id| *id > i64::from(cursor)).collect::<Vec<_>>(),
                            "states={states:?}, years={years:?}, cursor={cursor}"
                        );
                    }
                }
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}
