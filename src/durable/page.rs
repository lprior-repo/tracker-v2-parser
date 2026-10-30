pub(crate) mod artifacts;
mod binding;
mod execution;
mod failures;
mod offline;
mod retained;
mod retry;
mod selection;
mod stopped;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::model::Target;

pub use binding::{request, validate_request};
pub use execution::{fetch, parse, persist};
pub use failures::classify;
pub use offline::{bind_target, offline_replay};
pub(crate) use retry::{RetryPolicy, fetch_configured};
pub(crate) use selection::{Scheduling, reserve_configured, select_scheduled};
pub use selection::{reserve, select, select_excluding};

pub(crate) fn body_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn capture_digest(capture: &CaptureRef) -> anyhow::Result<String> {
    binding::digest(capture)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageScope {
    pub run_generation: String,
    pub config_sha256: String,
    pub deadline_ms: i64,
    pub cooldown_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageBinding {
    pub operation_id: String,
    pub page_id: i64,
    pub target_sha256: String,
    pub scope: PageScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    pub binding: PageBinding,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageReservation {
    pub request: PageRequest,
    pub attempt_id: String,
    pub cycle: i32,
    pub ordinal: i32,
    pub total_attempts: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRef {
    pub body: ArtifactRef,
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub retained_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedRef {
    pub body: ArtifactRef,
    pub capture_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PageOutcome {
    Completed {
        page_id: i64,
        receipt_sha256: String,
    },
    Deferred {
        retry_at_ms: i64,
        cycle: i32,
    },
    Blocked {
        detail: String,
    },
    Rejected {
        detail: String,
    },
    Exhausted {
        detail: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReservationDecision {
    Reserved { reservation: Box<PageReservation> },
    Settled { outcome: PageOutcome },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FetchDecision {
    Captured { capture: CaptureRef },
    Settled { outcome: PageOutcome },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ParseDecision {
    Parsed { parsed: ParsedRef },
    Settled { outcome: PageOutcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Transient,
    Blocked,
    Rejected,
}

pub struct FailureRecord<'a> {
    pub class: FailureClass,
    pub detail: &'a str,
    pub capture: Option<&'a crate::model::Capture>,
    pub stopped_origin: Option<&'a str>,
    pub observed_at_ms: i64,
}

pub(crate) fn attempt_identity(
    request: &PageRequest,
    cycle: i32,
    ordinal: i32,
) -> anyhow::Result<String> {
    binding::attempt_id(request, cycle, ordinal)
}

pub(crate) fn value_digest<T: serde::Serialize>(value: &T) -> anyhow::Result<String> {
    binding::digest(value)
}

pub(crate) fn transient_detail(detail: &str) -> bool {
    failures::transient_detail(detail)
}
