use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::runtime::Runtime;

pub(super) const HEADERS: [&str; 18] = [
    "Checkpoint ID",
    "Source",
    "Kind",
    "State",
    "School",
    "School source ID",
    "Athlete",
    "Athlete source ID",
    "URL",
    "Status",
    "Attempts",
    "Durable state",
    "Retry after UTC",
    "Error",
    "Last success",
    "Created",
    "Next action",
    "AI guidance",
];
const GUIDANCE: &str = "Use public evidence and exact source IDs. Do not bypass login, paywalls, challenges or saved stops; do not infer missing results.";

pub(super) async fn seed(runtime: &Runtime) -> Result<Vec<Vec<String>>> {
    use futures::{TryStreamExt, stream};
    let examples = [
        (
            "b",
            "failed",
            "Owned Wisconsin Track Fixture",
            "review-track-school",
            "https://review-fixture.invalid/wisconsin/contact-failure?program=track",
            "Contact parser rejected the isolated school fixture.",
            "Investigate checkpoint and retained evidence before an authorized retry.",
        ),
        (
            "c",
            "blocked",
            "Owned Wisconsin XC Fixture",
            "review-xc-school",
            "https://review-fixture.invalid/wisconsin/contact-challenge?program=xc",
            "Access denied: public page challenge requires human handling.",
            "Human handling required; preserve the access stop.",
        ),
    ];
    stream::iter(examples.into_iter().map(Ok::<_,anyhow::Error>)).try_fold(Vec::new(),|mut rows,(key,status,school,school_id,url,error,action)|async move {
        let created="2026-10-29T12:34:56.123456Z";
        let target=json!({"source":"milesplit","kind":"contacts","url":url,"context":{"state":"WI","school_id":school_id,"school_name":school,"season":2026}});
        let checkpoint:i64=sqlx::query_scalar("INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error,created_at) VALUES('milesplit',repeat($1,64),$2,$3,$4,0,$5,$6::timestamptz) RETURNING id")
            .bind(key).bind(url).bind(target).bind(status).bind(error).bind(created).fetch_one(&runtime.pool).await?;
        rows.push(vec![checkpoint.to_string(),"milesplit".into(),"contacts".into(),"WI".into(),school.into(),school_id.into(),String::new(),String::new(),url.into(),status.into(),"0".into(),String::new(),String::new(),error.into(),String::new(),created.into(),action.into(),GUIDANCE.into()]);
        Ok(rows)
    }).await
}

pub(super) fn verify(actual: &[Vec<String>], expected: &[Vec<String>]) -> Result<Value> {
    ensure!(
        expected.len() == 2
            && expected
                .first()
                .and_then(|row| row.get(9))
                .is_some_and(|status| status == "failed")
            && expected
                .last()
                .and_then(|row| row.get(9))
                .is_some_and(|status| status == "blocked"),
        "Review proof requires failed and blocked checkpoints"
    );
    ensure!(
        actual.len() == 3
            && actual
                .first()
                .is_some_and(|row| row.iter().map(String::as_str).eq(HEADERS)),
        "Review headers or row count differ"
    );
    ensure!(
        actual.get(1..) == Some(expected),
        "Review full-cell comparison failed"
    );
    ensure!(
        actual
            .iter()
            .flatten()
            .all(|value| !value.contains(['<', '>'])),
        "Review contains response markup"
    );
    let records = expected
        .iter()
        .map(|row| {
            HEADERS
                .iter()
                .zip(row)
                .map(|(key, value)| ((*key).to_owned(), json!(value)))
                .collect::<serde_json::Map<_, _>>()
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"rows_including_header":3,"headers":HEADERS,"checkpoints":records,"context_only_school_names_preserved":true,"no_raw_response_markup":true}),
    )
}
