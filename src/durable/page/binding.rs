use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{PageRequest, PageScope};
use crate::{database::Job, transport::validate_target};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PageBindingInput")]
pub struct PageBinding {
    operation_id: String,
    page_id: i64,
    target_sha256: String,
    scope: PageScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageBindingInput {
    operation_id: String,
    page_id: i64,
    target_sha256: String,
    scope: PageScope,
}

impl PageBinding {
    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    #[must_use]
    pub const fn page_id(&self) -> i64 {
        self.page_id
    }

    #[must_use]
    pub fn target_sha256(&self) -> &str {
        &self.target_sha256
    }

    #[must_use]
    pub const fn scope(&self) -> &PageScope {
        &self.scope
    }

    fn for_job(job: &Job, scope: &PageScope) -> Result<Self> {
        validate_scope(scope)?;
        validate_target(&job.target)?;
        let target_sha256 = digest(&job.target)?;
        let operation_id = Self::operation_identity(job.id, &target_sha256, scope)?;
        Ok(Self {
            operation_id,
            page_id: job.id,
            target_sha256,
            scope: scope.clone(),
        })
    }

    fn validate_target(&self, target: &crate::model::Target) -> Result<()> {
        validate_target(target)?;
        ensure!(
            digest(target)? == self.target_sha256,
            "durable page request binding mismatch"
        );
        Ok(())
    }

    fn operation_identity(page_id: i64, target_sha256: &str, scope: &PageScope) -> Result<String> {
        ensure!(page_id > 0, "durable checkpoint ID must be positive");
        digest(&(page_id, target_sha256, scope))
    }
}

impl TryFrom<PageBindingInput> for PageBinding {
    type Error = anyhow::Error;

    fn try_from(input: PageBindingInput) -> Result<Self> {
        validate_scope(&input.scope)?;
        validate_digest(&input.target_sha256)?;
        validate_digest(&input.operation_id)?;
        let expected = Self::operation_identity(input.page_id, &input.target_sha256, &input.scope)?;
        ensure!(
            input.operation_id == expected,
            "durable page binding operation ID mismatch"
        );
        Ok(Self {
            operation_id: input.operation_id,
            page_id: input.page_id,
            target_sha256: input.target_sha256,
            scope: input.scope,
        })
    }
}

pub(super) fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

pub(super) fn validate_digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "invalid durable SHA-256 digest"
    );
    Ok(())
}

pub(super) fn validate_scope(scope: &PageScope) -> Result<()> {
    crate::durable::retry_lineage::validate(
        crate::durable::retry_lineage::campaign(&scope.run_generation),
        &scope.inherited_retry_campaigns,
    )?;
    validate_digest(&scope.config_sha256)?;
    ensure!(
        !scope.run_generation.is_empty()
            && scope.run_generation.len() <= 128
            && scope
                .run_generation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "invalid durable run generation"
    );
    ensure!(
        scope.deadline_ms > 0 && (15_000..=3_600_000).contains(&scope.cooldown_ms),
        "invalid durable deadline or cooldown"
    );
    Ok(())
}

pub fn request(job: Job, scope: &PageScope) -> Result<PageRequest> {
    let binding = PageBinding::for_job(&job, scope)?;
    Ok(PageRequest {
        binding,
        target: job.target,
        deadline_extension: None,
    })
}

pub fn validate_request(value: &PageRequest) -> Result<()> {
    value.binding.validate_target(&value.target)?;
    if let Some(extension) = &value.deadline_extension {
        ensure!(
            extension.owner_operation_id == value.binding.operation_id(),
            "deadline extension owner does not match immutable operation"
        );
        ensure!(
            extension.previous_deadline_ms == value.binding.scope().deadline_ms
                && extension.effective_deadline_ms > extension.previous_deadline_ms,
            "invalid durable deadline extension"
        );
    }
    Ok(())
}

pub(super) fn attempt_id(request: &PageRequest, cycle: i32, ordinal: i32) -> Result<String> {
    ensure!(
        cycle > 0 && (1..=3).contains(&ordinal),
        "invalid durable attempt identity"
    );
    digest(&(request.binding.operation_id(), cycle, ordinal))
}

pub(super) fn deadline(scope: &PageScope, now_ms: i64) -> Result<()> {
    let remaining = scope
        .deadline_ms
        .checked_sub(now_ms)
        .context("durable deadline overflow")?;
    ensure!(
        (1..=86_400_000).contains(&remaining),
        "durable run deadline must be within the next 24 hours"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PageBinding, digest, request, validate_request};
    use crate::{
        database::Job,
        durable::page::{PageDeadlineExtension, PageRequest, PageScope},
        model::{PageContext, PageKind, Source, Target},
    };
    use anyhow::{Context, Result};

    fn legacy_request() -> Result<PageRequest> {
        let job = Job {
            id: 12,
            target: Target::get(
                Source::MileSplit,
                PageKind::Team,
                "https://wi.milesplit.com/teams/731-school".into(),
                PageContext {
                    state: Some("WI".into()),
                    school_id: Some("731".into()),
                    ..Default::default()
                },
            ),
        };
        let scope = PageScope {
            run_generation: "legacy-page-request".into(),
            config_sha256: "a".repeat(64),
            deadline_ms: 2_000_000_000_000,
            cooldown_ms: 60_000,
            inherited_retry_campaigns: Vec::new(),
        };
        request(job, &scope)
    }

    #[test]
    fn page_request_without_extension_decodes_as_legacy_input() -> Result<()> {
        let current = legacy_request()?;
        let expected = concat!(
            r#"{"operation_id":"03b176a5db04977284310f1f41aca1ebe133adda95eaa81b8505d6f02363dfa0","#,
            r#""page_id":12,"target_sha256":"e4a592e81eb0b2f46400194b19c5a9a84229d154d3e7425670efc942df3b5f05","#,
            r#""scope":{"run_generation":"legacy-page-request","#,
            r#""config_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
            r#""deadline_ms":2000000000000,"cooldown_ms":60000}}"#
        );
        assert_eq!(serde_json::to_string(&current.binding)?, expected);
        assert_eq!(
            serde_json::from_str::<PageBinding>(expected)?,
            current.binding
        );
        let mut serialized = serde_json::to_value(current)?;
        serialized
            .as_object_mut()
            .context("page request object")?
            .remove("deadline_extension");
        let decoded: PageRequest = serde_json::from_value(serialized)?;
        assert!(decoded.deadline_extension.is_none());
        validate_request(&decoded)?;
        let deadline_ms = decoded.binding.scope().deadline_ms;
        let mut wrong_owner = decoded;
        wrong_owner.deadline_extension = Some(PageDeadlineExtension {
            owner_operation_id: "b".repeat(64),
            previous_deadline_ms: deadline_ms,
            effective_deadline_ms: deadline_ms + 1,
        });
        assert!(
            validate_request(&wrong_owner)
                .err()
                .is_some_and(|error| error.to_string().contains("owner does not match"))
        );
        Ok(())
    }

    #[test]
    fn decoded_binding_rejects_noncanonical_operation_identity() -> Result<()> {
        let request = legacy_request()?;
        for (operation_id, message) in [
            ("b".repeat(64), "durable page binding operation ID mismatch"),
            (
                request.binding.operation_id().to_ascii_uppercase(),
                "invalid durable SHA-256 digest",
            ),
        ] {
            let mut wire = serde_json::to_value(&request.binding)?;
            wire["operation_id"] = serde_json::json!(operation_id);
            let error = serde_json::from_value::<PageBinding>(wire).expect_err(message);
            assert_eq!(error.to_string(), message);
        }
        Ok(())
    }

    #[test]
    fn decoded_binding_checks_bound_fields_even_with_matching_operation_identity() -> Result<()> {
        let request = legacy_request()?;
        let target_sha256 = request.binding.target_sha256();
        for (page_id, target_sha256, bad_scope_field, message) in [
            (
                0_i64,
                target_sha256,
                None,
                "durable checkpoint ID must be positive",
            ),
            (12, &"B".repeat(64), None, "invalid durable SHA-256 digest"),
            (
                12,
                target_sha256,
                Some(("config_sha256", serde_json::json!("A".repeat(64)))),
                "invalid durable SHA-256 digest",
            ),
            (
                12,
                target_sha256,
                Some(("run_generation", serde_json::json!("bad generation"))),
                "invalid durable run generation",
            ),
            (
                12,
                target_sha256,
                Some(("deadline_ms", serde_json::json!(0))),
                "invalid durable deadline or cooldown",
            ),
            (
                12,
                target_sha256,
                Some((
                    "inherited_retry_campaigns",
                    serde_json::json!(["legacy-page-request"]),
                )),
                "retry lineage cannot inherit its current campaign",
            ),
        ] {
            let mut scope = serde_json::to_value(request.binding.scope())?;
            if let Some((field, value)) = bad_scope_field {
                scope[field] = value;
            }
            let scope: PageScope = serde_json::from_value(scope)?;
            let operation_id = digest(&(page_id, target_sha256, &scope))?;
            let wire = serde_json::json!({
                "operation_id": operation_id,
                "page_id": page_id,
                "target_sha256": target_sha256,
                "scope": scope,
            });
            let error = serde_json::from_value::<PageBinding>(wire).expect_err(message);
            assert_eq!(error.to_string(), message);
        }
        Ok(())
    }
}
