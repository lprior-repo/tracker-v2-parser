use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    config::Config,
    durable::page::{PageRequest, RetryPolicy},
    model::{Source, Target},
    scope::LOWER_48,
    transport::{self, AccessStop, robots},
};

pub struct ScopeExclusion {
    config_sha256: String,
    target_sha256: String,
    reason: &'static str,
}

impl ScopeExclusion {
    pub fn bind(config: &Config, request: &PageRequest) -> Result<Option<Self>> {
        RetryPolicy::configured(config, request)?;
        Ok(config
            .acquisition
            .exclusion_reason(&request.target)
            .map(|reason| Self {
                config_sha256: request.binding.scope.config_sha256.clone(),
                target_sha256: request.binding.target_sha256.clone(),
                reason,
            }))
    }

    pub fn check(&self, request: &PageRequest) -> Result<()> {
        ensure!(
            self.config_sha256 == request.binding.scope.config_sha256
                && self.target_sha256 == request.binding.target_sha256,
            "durable scope exclusion binding mismatch"
        );
        Ok(())
    }

    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

pub struct PreservedStop<'a> {
    pub source: Source,
    pub address: &'a str,
    pub attempts: i32,
    pub cursor: &'a Value,
    pub detail: Option<&'a str>,
    pub no_remote_evidence: bool,
}

impl PreservedStop<'_> {
    pub fn local_robots(&self, target: &Target) -> bool {
        if !self.no_remote_evidence
            || self.attempts < 1
            || target.source != Source::MileSplit
            || target.token_url.is_some()
            || target.browser_origin.is_some()
            || target.body.is_some()
            || target
                .method
                .as_deref()
                .is_some_and(|method| method != "GET")
            || transport::validate_target(target).is_err()
        {
            return false;
        }
        let Ok(url) = url::Url::parse(self.address) else {
            return false;
        };
        let site = url
            .host_str()
            .and_then(|host| host.strip_suffix(".milesplit.com"));
        if url.as_str() != self.address
            || url.scheme() != "https"
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.path().contains('%')
            || !site.is_some_and(|site| {
                site == "www" || LOWER_48.contains(&site.to_ascii_uppercase().as_str())
            })
        {
            return false;
        }
        let Err(error) = robots::check(self.address, self.address) else {
            return false;
        };
        let Some(stop) = error.downcast_ref::<AccessStop>() else {
            return false;
        };
        error.is::<robots::PublishedRestriction>()
            && stop.status.is_none()
            && self.cursor.get("access_stop_url").and_then(Value::as_str)
                == Some(stop.origin.as_str())
            && self
                .detail
                .is_some_and(|detail| detail == format!("{error:#}"))
    }
}

#[cfg(test)]
mod tests;
