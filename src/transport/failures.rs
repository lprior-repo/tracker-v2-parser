use std::fmt;

use crate::model::Capture;

#[derive(Debug)]
#[non_exhaustive]
pub struct Failure {
    pub detail: Box<str>,
    kind: FailureKind,
}

#[derive(Debug)]
enum FailureKind {
    AccessStopped {
        origin: Option<Box<str>>,
        capture: Option<Capture>,
    },
    Retryable,
    Permanent {
        capture: Option<Capture>,
    },
}

impl Failure {
    #[must_use]
    pub fn blocked(
        detail: impl Into<Box<str>>,
        origin: Option<Box<str>>,
        capture: Option<Capture>,
    ) -> Self {
        Self {
            detail: detail.into(),
            kind: FailureKind::AccessStopped { origin, capture },
        }
    }

    #[must_use]
    pub fn retryable(detail: impl Into<Box<str>>) -> Self {
        Self {
            detail: detail.into(),
            kind: FailureKind::Retryable,
        }
    }

    #[must_use]
    pub fn permanent(detail: impl Into<Box<str>>, capture: Option<Capture>) -> Self {
        Self {
            detail: detail.into(),
            kind: FailureKind::Permanent { capture },
        }
    }

    #[must_use]
    pub const fn capture(&self) -> Option<&Capture> {
        match &self.kind {
            FailureKind::AccessStopped { capture, .. } | FailureKind::Permanent { capture } => {
                capture.as_ref()
            }
            FailureKind::Retryable => None,
        }
    }

    #[must_use]
    pub fn into_capture(self) -> Option<Capture> {
        match self.kind {
            FailureKind::AccessStopped { capture, .. } | FailureKind::Permanent { capture } => {
                capture
            }
            FailureKind::Retryable => None,
        }
    }

    #[must_use]
    pub const fn is_blocked(&self) -> bool {
        matches!(self.kind, FailureKind::AccessStopped { .. })
    }

    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self.kind, FailureKind::Retryable)
    }

    #[must_use]
    pub fn stopped_origin(&self) -> Option<&str> {
        match &self.kind {
            FailureKind::AccessStopped { origin, .. } => origin.as_deref(),
            FailureKind::Retryable | FailureKind::Permanent { .. } => None,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for Failure {}

#[derive(Debug, Clone)]
pub struct AccessStop {
    pub origin: String,
    pub status: Option<u16>,
}

impl fmt::Display for AccessStop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source access stopped at {} (HTTP {:?}); human review required",
            self.origin, self.status
        )
    }
}

impl std::error::Error for AccessStop {}

#[derive(Debug)]
pub struct AccessStopCaptureError {
    pub(crate) origin: Option<String>,
    pub(crate) status: u16,
    pub(crate) error: anyhow::Error,
}

impl AccessStopCaptureError {
    pub(crate) fn new(status: u16, error: anyhow::Error, address: &str) -> Self {
        let origin = url::Url::parse(address)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .map(|url| url.origin().ascii_serialization());
        Self {
            origin,
            status,
            error,
        }
    }
}

impl fmt::Display for AccessStopCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source returned HTTP {}; capturing response failed: {:#}",
            self.status, self.error
        )
    }
}

impl std::error::Error for AccessStopCaptureError {}

pub fn failure(error: &anyhow::Error) -> Failure {
    let detail = format!("{error:#}");
    let stopped = error.downcast_ref::<AccessStop>();
    let capture_stop = error.downcast_ref::<AccessStopCaptureError>();
    if stopped.is_some() || capture_stop.is_some() || access_stop_marker(&detail) {
        let origin = stopped
            .map(|stop| stop.origin.as_str())
            .or_else(|| capture_stop.and_then(|stop| stop.origin.as_deref()));
        return Failure::blocked(detail, origin.map(Box::from), None);
    }
    if crate::durable::retryable_error(error) {
        return Failure::retryable(detail);
    }
    Failure::permanent(detail, None)
}

fn access_stop_marker(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    [
        "challenge",
        "requires authentication",
        "paywall",
        "human review",
        "human handling",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}
