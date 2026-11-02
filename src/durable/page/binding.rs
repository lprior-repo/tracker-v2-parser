use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{PageBinding, PageRequest, PageScope};
use crate::{database::Job, transport::validate_target};

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
    validate_scope(scope)?;
    validate_target(&job.target)?;
    ensure!(job.id > 0, "durable checkpoint ID must be positive");
    let target_sha256 = digest(&job.target)?;
    let operation_id = digest(&(job.id, &target_sha256, scope))?;
    Ok(PageRequest {
        binding: PageBinding {
            operation_id,
            page_id: job.id,
            target_sha256,
            scope: scope.clone(),
        },
        target: job.target,
        deadline_extension: None,
    })
}

pub fn validate_request(value: &PageRequest) -> Result<()> {
    let expected = request(
        Job {
            id: value.binding.page_id,
            target: value.target.clone(),
        },
        &value.binding.scope,
    )?;
    ensure!(
        expected.binding == value.binding,
        "durable page request binding mismatch"
    );
    if let Some(extension) = &value.deadline_extension {
        ensure!(
            extension.owner_operation_id == value.binding.operation_id,
            "deadline extension owner does not match immutable operation"
        );
        ensure!(
            extension.previous_deadline_ms == value.binding.scope.deadline_ms
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
    digest(&(&request.binding.operation_id, cycle, ordinal))
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
    use super::{request, validate_request};
    use crate::{
        database::Job,
        durable::page::{PageDeadlineExtension, PageRequest, PageScope},
        model::{PageContext, PageKind, Source, Target},
    };
    use anyhow::{Context, Result};

    #[test]
    fn page_request_without_extension_decodes_as_legacy_input() -> Result<()> {
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
        let current = request(job, &scope)?;
        let mut serialized = serde_json::to_value(current)?;
        serialized
            .as_object_mut()
            .context("page request object")?
            .remove("deadline_extension");
        let decoded: PageRequest = serde_json::from_value(serialized)?;
        assert!(decoded.deadline_extension.is_none());
        validate_request(&decoded)?;
        let mut wrong_owner = decoded;
        wrong_owner.deadline_extension = Some(PageDeadlineExtension {
            owner_operation_id: "b".repeat(64),
            previous_deadline_ms: scope.deadline_ms,
            effective_deadline_ms: scope.deadline_ms + 1,
        });
        assert!(
            validate_request(&wrong_owner)
                .err()
                .is_some_and(|error| error.to_string().contains("owner does not match"))
        );
        Ok(())
    }
}
