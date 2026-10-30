use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::AcquisitionPolicy;
use crate::{config::Config, durable::page::PageRequest, model::PageKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewedLegacyFailure {
    WiaaScriptSanitization,
}

pub struct LegacyFailurePermit {
    config_sha256: String,
}

impl LegacyFailurePermit {
    pub(crate) fn bind(config: &Config, request: &PageRequest) -> Result<Option<Self>> {
        let mut canonical = config.clone();
        canonical.validate()?;
        let digest = crate::durable::model::page_config_digest(config)?;
        ensure!(
            crate::durable::model::page_config_digest(&canonical)? == digest
                && digest == request.binding.scope.config_sha256,
            "durable configuration binding mismatch"
        );
        Ok(matches!(&config.acquisition, AcquisitionPolicy::PublicResults { reviewed_legacy_failures, collect_contacts: true, .. }
            if reviewed_legacy_failures.contains(&ReviewedLegacyFailure::WiaaScriptSanitization))
            .then_some(Self { config_sha256: digest }))
    }

    pub(crate) fn permits(&self, request: &PageRequest, detail: &str) -> bool {
        let lower = detail.to_ascii_lowercase();
        self.config_sha256 == request.binding.scope.config_sha256
            && request.target.kind == PageKind::Contacts
            && crate::sources::wiaa::is_public_directory_url(&request.target.url)
            && detail.starts_with("scanning source script boundaries: cannot safely classify computed script assignments")
            && !["http 403", "status=403", "status 403", "status: 403", "login", "captcha", "access stop", "access denied", "challenge_marker=true"]
                .iter().any(|marker| lower.contains(marker))
    }
}

#[cfg(test)]
mod tests;
