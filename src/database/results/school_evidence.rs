use anyhow::{Result, anyhow};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use sqlx::PgConnection;
use url::Url;

use crate::model::{Performance, Source};

pub(super) struct Replay {
    pub clear: bool,
    pub evidence: Option<Value>,
}

#[tracing::instrument(skip(connection, performance))]
pub(super) async fn resolve(
    connection: &mut PgConnection,
    source: Source,
    performance: &Performance,
    key: &str,
) -> Result<Replay> {
    let retained = Replay {
        clear: false,
        evidence: None,
    };
    let Some(values) = performance.source_values.as_ref() else {
        return Ok(retained);
    };
    if source != Source::MileSplit
        || performance.school_id.is_some()
        || values.get("school_attribution").and_then(Value::as_str) != Some("not_published")
        || !profile_row(values)?
    {
        return Ok(retained);
    }
    let previous: Option<(Value, String, Option<String>)> = sqlx::query_as(
        "SELECT r.raw,r.source_url,s.source_id FROM results r \
         LEFT JOIN schools s ON s.id=r.represented_school_id AND s.source='milesplit' \
         WHERE r.source_result_key=$1 FOR UPDATE OF r",
    )
    .bind(key)
    .fetch_optional(connection)
    .await?;
    let Some((raw, location, school)) = previous else {
        return Ok(retained);
    };
    let Some(school) = school else {
        return Ok(retained);
    };
    let absent = Value::Null;
    let previous_values = raw.get("source_values").map_or(&absent, |values| values);
    if previous_values
        .get("school_attribution")
        .and_then(Value::as_str)
        == Some("unverified_prior")
    {
        return Ok(unverified(previous_values, &location, &school));
    }
    let Some(base) = profile_url(&location, &raw) else {
        return Ok(unverified(previous_values, &location, &school));
    };
    if let Some(evidence) = accepted_evidence(previous_values, &base, &school)? {
        return Ok(Replay {
            clear: false,
            evidence: Some(json!({
                "school_attribution": "row_link", "school_evidence": evidence
            })),
        });
    }
    if profile_row(previous_values)? {
        return Ok(Replay {
            clear: true,
            evidence: None,
        });
    }
    Ok(unverified(previous_values, &location, &school))
}

fn unverified(values: &Value, location: &str, school: &str) -> Replay {
    let prior = values.get("unverified_school_evidence").map_or_else(
        || json!({"school_id": school, "url": location, "source_values": values}),
        Clone::clone,
    );
    Replay {
        clear: false,
        evidence: Some(json!({
            "school_attribution": "unverified_prior", "unverified_school_evidence": prior
        })),
    }
}

pub(super) fn profile_url(location: &str, raw: &Value) -> Option<Url> {
    let url = Url::parse(location).ok()?;
    let athlete = raw.get("athlete_id")?.as_str()?;
    (public_milesplit(&url) && path_id(&url, "athletes") == Some(athlete)).then_some(url)
}

fn public_milesplit(url: &Url) -> bool {
    matches!(url.scheme(), "https" | "http")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|host| host == "milesplit.com" || host.ends_with(".milesplit.com"))
}

fn path_id<'a>(url: &'a Url, collection: &str) -> Option<&'a str> {
    let remainder = url
        .path()
        .strip_prefix('/')?
        .strip_prefix(collection)?
        .strip_prefix('/')?;
    let id = remainder.split('/').next()?.split('-').next()?;
    (!id.is_empty()
        && !id.starts_with('0')
        && id.bytes().all(|byte| byte.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|value| value > 0))
    .then_some(id)
}

fn profile_row(values: &Value) -> Result<bool> {
    let Some(html) = values.pointer("/row/html").and_then(Value::as_str) else {
        return Ok(false);
    };
    let selector = match values.get("kind").and_then(Value::as_str) {
        Some("personal_record") => "tr[data-pr-mode][data-pr-level]",
        Some("career_result") => ".record[data-season][data-event]",
        _ => return Ok(false),
    };
    let document = Html::parse_fragment(&format!("<table>{html}</table>"));
    let selector = Selector::parse(selector)
        .map_err(|error| anyhow!("invalid profile row selector: {error}"))?;
    Ok(document.select(&selector).take(2).count() == 1)
}

fn accepted_evidence(values: &Value, base: &Url, school: &str) -> Result<Option<Value>> {
    let recorded = values
        .get("school_evidence")
        .filter(|evidence| evidence.get("school_id").and_then(Value::as_str) == Some(school));
    if let Some(evidence) = recorded
        && matching_link(evidence, base, school)?
    {
        return Ok(Some(evidence.clone()));
    }
    if !profile_row(values)? {
        return Ok(None);
    }
    let Some(row) = values.get("row") else {
        return Ok(None);
    };
    if !matching_link(row, base, school)? {
        return Ok(None);
    }
    let evidence = row.as_object().map(|object| {
        Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .chain(std::iter::once(("school_id".to_owned(), json!(school))))
                .collect(),
        )
    });
    Ok(evidence)
}

fn matching_link(evidence: &Value, base: &Url, school: &str) -> Result<bool> {
    let Some(html) = evidence.get("html").and_then(Value::as_str) else {
        return Ok(false);
    };
    let document = Html::parse_fragment(&format!("<table>{html}</table>"));
    let selector = Selector::parse("a[href*='/teams/']")
        .map_err(|error| anyhow!("invalid school link selector: {error}"))?;
    let link = document
        .select(&selector)
        .next()
        .and_then(|link| link.value().attr("href"))
        .and_then(|href| base.join(href).ok());
    Ok(link.is_some_and(|url| public_milesplit(&url) && path_id(&url, "teams") == Some(school)))
}
