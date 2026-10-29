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
