mod support;

use anyhow::Result;
use sqlx::PgPool;
use track_scrapper_v2::{database::Database, export::ReviewStatus};

const WI: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 10];
const MI: &[i64] = &[8, 9];
const ALL: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

fn url(id: i64) -> String {
    format!("https://co.milesplit.com/athletes/{id}/stats")
}

async fn seed(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        r"INSERT INTO schools(source,source_id,name,state,source_url) VALUES
            ('milesplit','1000','Wisconsin High','WI','https://wi.milesplit.com/teams/1000'),
            ('milesplit','1001','Michigan High','MI','https://mi.milesplit.com/teams/1001'),
            ('milesplit','1002','Unconfirmed High',U&'\2003\00A0','https://co.milesplit.com/teams/1002');
        INSERT INTO scrape_pages(id,source,page_key,url,cursor,status,attempts,last_error)
        OVERRIDING SYSTEM VALUE
        SELECT id,'milesplit',lpad(to_hex(id),64,'0'),
               'https://co.milesplit.com/athletes/'||id||'/stats',
               jsonb_build_object('kind','athlete','context',jsonb_strip_nulls(jsonb_build_object(
                   'acquisition_state',acquisition,'state',state,'school_id',school))),
               'failed',1,'Scope fallback '||id
        FROM (VALUES
            (1,NULL,'WI','1001'),
            (2,'','WI','1001'),
            (3,'   ','WI','1001'),
            (4,U&'\0009\000D','WI','1001'),
            (5,U&'\0085\00A0\2003\3000','WI','1001'),
            (6,NULL,NULL,'1000'),
            (7,U&'\2003\00A0',U&'\0009\3000','1000'),
            (8,U&'\2003mI\00A0','WI','1000'),
            (9,NULL,'MI','1000'),
            (10,'','', '1000')
        ) c(id,acquisition,state,school);",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_review_fallbacks(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        r"WITH athlete AS (
            INSERT INTO athletes(name)
            SELECT 'Synthetic Athlete '||id FROM generate_series(11,13) id RETURNING id,name
        )
        INSERT INTO athlete_sources(athlete_id,source,source_id,school_id,state)
        SELECT athlete.id,'milesplit',claim.source_id,schools.id,claim.state FROM athlete
        CROSS JOIN (VALUES ('11','1000','MI'),('12','1002','MI'),('13','1002',U&'\2003\00A0'))
            claim(source_id,school_id,state)
        JOIN schools ON schools.source='milesplit' AND schools.source_id=claim.school_id
        WHERE athlete.name='Synthetic Athlete '||claim.source_id;
        INSERT INTO scrape_pages(id,source,page_key,url,cursor,status,attempts,last_error)
        OVERRIDING SYSTEM VALUE
        SELECT id,'milesplit',lpad(to_hex(id),64,'0'),
               'https://co.milesplit.com/athletes/'||id||'/stats',
               jsonb_build_object('kind','athlete','context',jsonb_build_object(
                   'acquisition_state',U&'\0009\2003','state',U&'\00A0\3000',
                   'school_id','1002','athlete_id',id::text)),
               'failed',1,'Scope fallback '||id
        FROM generate_series(11,13) id;",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn given_blank_optional_scope_when_exporting_coverage_then_context_or_school_fallback_is_preserved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(database.pool()).await?;
            for (states, ids) in [
                (vec!["WI".into()], WI),
                (vec!["MI".into()], MI),
                (vec![], ALL),
            ] {
                let (athletes, issues) = database.export_snapshot(&states, &[2028]).await?;
                assert!(athletes.is_empty());
                let actual = issues
                    .into_iter()
                    .map(|issue| (issue.source, issue.url, issue.detail))
                    .collect::<Vec<_>>();
                let expected = ids
                    .iter()
                    .map(|id| {
                        (
                            String::from("milesplit"),
                            url(*id),
                            format!("page failed: Scope fallback {id}"),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "Coverage scope {states:?}");
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_blank_optional_scope_when_reviewing_then_context_or_school_fallback_is_preserved()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            seed(database.pool()).await?;
            seed_review_fallbacks(database.pool()).await?;
            for (state, ids) in [
                ("WI", [WI, &[11]].concat()),
                ("MI", [MI, &[12]].concat()),
                ("CO", vec![13]),
            ] {
                let mut snapshot = database
                    .begin_export_snapshot(&[state.into()], &[2028])
                    .await?;
                let rows = snapshot.review_records(0, 1000).await?;
                let actual = rows
                    .iter()
                    .map(|row| {
                        (
                            row.checkpoint_id,
                            row.state.as_str(),
                            row.url.clone(),
                            row.status,
                            row.error.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                let expected = ids
                    .iter()
                    .map(|id| {
                        (
                            *id,
                            state,
                            url(*id),
                            ReviewStatus::Failed,
                            format!("Scope fallback {id}"),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "Review scope {state}");
                snapshot.finish().await?;
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}
