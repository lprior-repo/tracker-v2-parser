use serde_json::Value;

use super::values as v;

pub(super) fn definition<'a>(body: &'a Value, block: &Value, row: &Value) -> Option<&'a Value> {
    let event = row.get("EventID").or_else(|| block.get("EventId"))?;
    let kind = row
        .get("EventTypeID")
        .or_else(|| block.get("IDEventType"))?;
    body.get("eventTypes")?
        .as_array()?
        .iter()
        .find(|definition| {
            definition.get("eventId") == Some(event) && definition.get("IDEventType") == Some(kind)
        })
}

pub(super) fn label(base: &str, definition: Option<&Value>) -> String {
    definition
        .and_then(|definition| v::text(definition, "Description"))
        .map_or_else(
            || base.to_owned(),
            |description| format!("{base} ({description})"),
        )
}

pub(super) fn evidence(block: &Value, row: &Value, definition: Option<&Value>) -> Value {
    serde_json::json!({
        "row": row,
        "event": {
            "Event": block.get("Event"),
            "EventId": block.get("EventId"),
            "IDEventType": block.get("IDEventType"),
            "DivId": block.get("DivId"),
            "Level": block.get("Level"),
            "LevelMask": block.get("LevelMask")
        },
        "eventType": definition
    })
}
