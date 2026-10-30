use anyhow::Result;

use super::{PageRequest, RetryPolicy};
use crate::{config::Config, model::Target};

pub struct IndexRefreshPermit {
    config_sha256: String,
}

impl IndexRefreshPermit {
    pub fn bind(config: &Config, request: &PageRequest) -> Result<Option<Self>> {
        RetryPolicy::configured(config, request)?;
        Ok((config.states.iter().any(|state| state == "WI")
            && config.acquisition.includes_contacts()
            && config.enabled_sources().contains(&request.target.source)
            && eligible(&request.target))
        .then(|| Self {
            config_sha256: request.binding.scope.config_sha256.clone(),
        }))
    }

    pub fn permits(&self, request: &PageRequest) -> bool {
        self.config_sha256 == request.binding.scope.config_sha256 && eligible(&request.target)
    }
}

fn eligible(target: &Target) -> bool {
    crate::durable::wiaa::is_index(target)
        && target.context.state.as_deref() == Some("WI")
        && target.body.is_none()
        && target.token_url.is_none()
        && target.browser_origin.is_none()
        && if target.url == "https://schools.wiaawi.org/Directory/School/List" {
            target
                .method
                .as_deref()
                .is_none_or(|method| method == "GET")
        } else {
            target.method.as_deref() == Some("POST")
                && target
                    .url
                    .strip_prefix(
                        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=",
                    )
                    .is_some_and(|letter| {
                        letter.len() == 1 && letter.bytes().all(|byte| byte.is_ascii_uppercase())
                    })
        }
}

#[cfg(test)]
mod tests;
