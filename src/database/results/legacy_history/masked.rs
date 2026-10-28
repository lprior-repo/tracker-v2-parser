use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use futures::{TryStreamExt, stream};
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use sqlx::PgConnection;

use crate::model::{Capture, PageKind, Performance, Target};

pub(super) async fn reconfirm(connection: &mut PgConnection, rows: &[Performance]) -> Result<()> {
    let ids = rows
        .iter()
        .filter_map(|row| {
            let kind = row.source_values.as_ref()?.get("kind")?.as_str()?;
            matches!(kind, "personal_record" | "career_result")
                .then_some(row.result_id.as_deref())
                .flatten()
        })
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        sqlx::query("UPDATE results SET raw=raw #- '{source_values,revision_status}' #- '{source_values,mask_reconciliation}' WHERE source='milesplit' AND source_result_id=ANY($1) AND raw#>>'{source_values,mask_reconciliation}'='retained_mark_masked'")
            .bind(ids).execute(connection).await?;
    }
    Ok(())
}

fn selector(value: &str) -> Result<Selector> {
    Selector::parse(value).map_err(|error| anyhow::anyhow!("invalid mask selector: {error}"))
}

fn mark_masked(row: ElementRef<'_>) -> Result<bool> {
    let mark = if row.value().name() == "tr" {
        row.select(&selector("td")?).nth(1)
    } else {
        row.select(&selector(".seed")?).next()
    };
    let Some(mark) = mark else {
        return Ok(false);
    };
    let masks = selector(".mask, .redact, .record--locked, .paywall, [aria-label='Locked']")?;
    Ok(masks.matches(&mark)
        || mark.select(&masks).next().is_some()
        || mark
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| masks.matches(&ancestor)))
}

fn masked_rows(capture: &Capture) -> Result<BTreeSet<String>> {
    ensure!(
        capture.body.len() <= 16 * 1024 * 1024,
        "mask replay capture exceeds 16 MiB"
    );
    let body = std::str::from_utf8(&capture.body).context("mask replay capture is not UTF-8")?;
    let document = Html::parse_document(body);
    document
        .select(&selector(
            "#prTable tr[data-pr-mode], #stats .record[data-season][data-event]",
        )?)
        .take(10001)
        .enumerate()
        .try_fold(BTreeSet::new(), |mut rows, (index, row)| {
            ensure!(index < 10000, "mask replay exceeds 10000 rows");
            if mark_masked(row)? {
                rows.insert(row.html());
            }
            ensure!(rows.len() <= 10000, "mask replay exceeds 10000 rows");
            Ok(rows)
        })
}

fn retained_masked(body: &str) -> Result<bool> {
    let document = Html::parse_document(&format!("<table>{body}</table>"));
    let rows = document
        .select(&selector("tr, .record")?)
        .collect::<Vec<_>>();
    match rows.as_slice() {
        [row] => mark_masked(*row),
        _ => Ok(false),
    }
}

pub(in crate::database::results) async fn review(
    connection: &mut PgConnection,
    target: &Target,
    capture: &Capture,
) -> Result<()> {
    if target.kind != PageKind::Athlete {
        return Ok(());
    }
    let masked = masked_rows(capture)?;
    let rows = sqlx::query_as::<_, (i64, String, Value)>(
        "SELECT r.id,r.source_url,r.raw FROM results r \
         WHERE r.source='milesplit' AND r.source_url=$1 \
         AND r.raw#>>'{source_values,kind}' IN ('career_result','personal_record') \
         ORDER BY r.id LIMIT 10001 FOR UPDATE",
    )
    .bind(&capture.url)
    .fetch_all(&mut *connection)
    .await?;
    ensure!(
        rows.len() <= 10000,
        "retained mask review exceeds 10000 results"
    );
    stream::iter(rows.into_iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, (id, location, raw)| {
            let masked = &masked;
            async move {
                let html = raw
                    .pointer("/source_values/row/html")
                    .and_then(Value::as_str);
                if let Some(html) = html
                    && super::school_evidence::profile_url(&location, &raw).is_some()
                {
                    let invalid = retained_masked(html)?;
                    if invalid || masked.contains(html) {
                        mark(connection, id, invalid).await?;
                    }
                }
                Ok(connection)
            }
        })
        .await
        .map(|_| ())
}

async fn mark(connection: &mut PgConnection, id: i64, invalid: bool) -> Result<()> {
    let reason = if invalid {
        "retained_mark_masked"
    } else {
        "ancestor_mask_capture_binding_unproven"
    };
    sqlx::query(
        "UPDATE results SET raw=jsonb_set(raw,'{source_values,mask_reconciliation}',to_jsonb($2::text),TRUE) WHERE id=$1",
    ).bind(id).bind(reason).execute(&mut *connection).await?;
    if invalid {
        super::set_revision(connection, id, false).await?;
    }
    sqlx::query("UPDATE athletes SET needs_review=TRUE WHERE id=(SELECT athlete_id FROM results WHERE id=$1)")
        .bind(id).execute(connection).await?;
    Ok(())
}
