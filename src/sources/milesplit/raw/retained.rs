use anyhow::{Result, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::model::UnassignedResult;

use super::{RowScope, columns};
use crate::sources::milesplit::{html, targets};

pub(super) fn result_id(scope: &RowScope<'_>) -> Result<String> {
    let meet = scope
        .context
        .subject_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("raw row has no meet identity"))?;
    let url = targets::public_url(&scope.target.url)?;
    let file = if url.path().trim_end_matches('/').ends_with("/results") {
        "inline".to_owned()
    } else {
        targets::path_id(&scope.target.url, "results")?
    };
    let coordinates = (
        meet,
        file,
        scope.event,
        scope.round,
        scope.ordinal.saturating_add(1),
        format!("{:x}", Sha256::digest(scope.line.as_bytes())),
    );
    Ok(format!("raw:{}", serde_json::to_string(&coordinates)?))
}

pub(super) fn unassigned(
    scope: &RowScope<'_>,
    row: Option<&columns::Row<'_>>,
    school: Option<&str>,
) -> Result<UnassignedResult> {
    let sport = scope
        .context
        .sport
        .ok_or_else(|| anyhow::anyhow!("raw row has no sport"))?;
    let raw = json!({
        "source_url": scope.target.url,
        "line_ordinal": scope.ordinal.saturating_add(1),
        "published_html": scope.raw,
        "published_text": scope.line,
        "name": row.map(|row| row.name),
        "grade": row.and_then(|row| row.grade),
        "wind": row.and_then(|row| row.wind),
        "event": scope.event,
        "round": scope.round,
        "meet_name": scope.context.meet_name,
        "column_layout_readable": row.is_some(),
    });
    ensure!(
        !scope.line.trim().is_empty(),
        "cannot retain an empty result line"
    );
    Ok(UnassignedResult {
        result_id: Some(result_id(scope)?),
        source_row: None,
        event: scope.event.to_owned(),
        mark: row.map_or("", |row| row.mark).to_owned(),
        sport,
        school_id: school.map(str::to_owned),
        meet_id: scope.context.subject_id.clone(),
        date: scope.context.meet_date.clone(),
        is_relay: html::relay(scope.event),
        members: Vec::new(),
        raw,
    })
}
