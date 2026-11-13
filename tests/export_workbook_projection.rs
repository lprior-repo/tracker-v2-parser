mod support;

use anyhow::{Context, Result};
use serde_json::{Value, json};

const SQL: &str = include_str!("../src/database/export_snapshot/workbook.sql");

fn expected(mut document: Value) -> Result<Value> {
    for profile in document["profile_details"]
        .as_array_mut()
        .context("profiles")?
    {
        let pages = if profile["source"] == "milesplit" {
            profile["source_values"]["pages"].as_object()
        } else {
            None
        };
        let pages = pages
            .into_iter()
            .flatten()
            .filter_map(|(url, page)| {
                let rows = page
                    .get("pr_rows")?
                    .as_array()?
                    .iter()
                    .filter_map(|row| {
                        let html = row.get("html")?.as_str()?;
                        Some(json!({"html":html,"masked":row.get("masked")}))
                    })
                    .collect::<Vec<_>>();
                (!rows.is_empty()).then(|| (url.clone(), json!({"pr_rows":rows})))
            })
            .collect::<serde_json::Map<_, _>>();
        profile["source_values"] = json!({"pages":pages});
    }
    for evidence in document["performances"]
        .as_array_mut()
        .context("performances")?
    {
        let fields = ["level", "revision_status"]
            .into_iter()
            .filter_map(|key| {
                evidence["performance"]["source_values"]
                    .get(key)
                    .map(|value| (key.to_owned(), value.clone()))
            })
            .collect::<serde_json::Map<_, _>>();
        evidence["performance"]["source_values"] = if fields.is_empty() {
            Value::Null
        } else {
            Value::Object(fields)
        };
    }
    Ok(document)
}

fn document() -> Value {
    json!({"id":1,"name":"Fixture","gender":"female","grade":11,"graduation_year":2027,
    "needs_review":false,"schools":[],"coaches":[],"profiles":[],
    "profile_details":[
      {"source":"milesplit","source_id":"1","url":null,"source_values":{"unused":"body","pages":{
        "first":{"pr_rows":[{"html":"<tr>visible</tr>"},{"html":"masked","masked":true,"extra":44},{"html":null},{"html":17},{"masked":false},null]},
        "second":{"pr_rows":{}},"third":{"pr_rows":[]},"fourth":{"pr_rows":[{"html":"","masked":null}]}
      }}},
      {"source":"athleticnet","source_id":"2","url":"https://fixture","source_values":{"pages":{"unused":{"pr_rows":[{"html":"discard"}]}}}},
      {"source":"milesplit","source_id":"3","url":null,"source_values":{"pages":null}}
    ],"performances":[
      {"source":"milesplit","url":"first","performance":{"mark":"1","source_values":{"level":null,"revision_status":"unverified_prior_revision","discard":"body"}}},
      {"source":"milesplit","url":"second","performance":{"mark":"2","source_values":null}},
      {"source":"athleticnet","url":"third","performance":{"mark":"3","source_values":["discard"]}}
    ]})
}

#[tokio::test]
async fn given_mixed_profile_shapes_when_projected_before_transfer_then_order_masks_nulls_and_original_bytes_match()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            let input = document();
            let expected = expected(input.clone())?;
            let query = SQL.replace(
                "__RECORDS__",
                "SELECT $1::jsonb WHERE $2::text IS NULL AND $3::text IS NULL",
            );
            let original: i64 = sqlx::query_scalar("SELECT octet_length($1::jsonb::text)::bigint")
                .bind(&input)
                .fetch_one(&test.pool)
                .await?;
            for budget in [original - 1, original, original + 1] {
                let (output, bytes): (Option<String>, i64) =
                    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
                        .bind(&input)
                        .bind(None::<&str>)
                        .bind(None::<&str>)
                        .bind(budget)
                        .fetch_one(&test.pool)
                        .await?;
                assert_eq!(bytes, original);
                if budget < original {
                    assert!(output.is_none());
                } else {
                    assert_eq!(
                        serde_json::from_str::<Value>(&output.context("projected document")?)?,
                        expected
                    );
                }
            }
            Ok(())
        })
    })
    .await
}
