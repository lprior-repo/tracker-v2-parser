use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{PrepareArgs, stage};
use crate::native_ops::resources::ConfigurationProfile;

fn fixture(root: &std::path::Path) -> Result<PrepareArgs> {
    fs::create_dir(root.join("live"))?;
    fs::create_dir(root.join("staging"))?;
    let prior = root.join("live/postgresql.conf");
    fs::write(&prior, b"shared_buffers='128MB'\n")?;
    fs::set_permissions(&prior, fs::Permissions::from_mode(0o600))?;
    Ok(PrepareArgs {
        database: "fixture".into(),
        prior_config: prior,
        expected_prior_sha256: format!("{:x}", Sha256::digest(b"shared_buffers='128MB'\n")),
        candidate_config: root.join("staging/candidate.conf"),
        output: root.join("staging/receipt.json"),
        profile: ConfigurationProfile::Observability,
        host_memory_gib: 16,
        stack_memory_gib: 8,
    })
}

#[test]
fn given_exact_private_configuration_when_candidate_is_staged_then_original_and_previous_receipts_are_preserved()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let args = fixture(root.path())?;
    let original = fs::read(&args.prior_config)?;
    stage(&args, &json!({"configuration":"fsync = 'on'\n"}))?;
    let receipt: Value = serde_json::from_slice(&fs::read(&args.output)?)?;
    assert_eq!(receipt["applied"], false);
    assert_eq!(fs::read(&args.prior_config)?, original);
    let candidate = fs::read(&args.candidate_config)?;
    assert_eq!(candidate, b"shared_buffers='128MB'\n\nfsync = 'on'\n");
    assert_eq!(
        receipt["candidate_configuration"]["sha256"],
        format!("{:x}", Sha256::digest(&candidate))
    );
    assert_eq!(
        fs::metadata(&args.candidate_config)?.permissions().mode() & 0o777,
        0o600
    );
    assert!(stage(&args, &json!({"configuration":"fsync = 'on'\n"})).is_err());
    assert_eq!(fs::read(&args.candidate_config)?, candidate);
    Ok(())
}

#[test]
fn given_wrong_prior_hash_or_live_target_when_staging_then_no_candidate_is_published() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let mut args = fixture(root.path())?;
    let hash = args.expected_prior_sha256.clone();
    args.expected_prior_sha256 = "0".repeat(64);
    assert!(stage(&args, &json!({"configuration":"fsync = 'on'\n"})).is_err());
    assert!(!args.candidate_config.exists());
    assert!(!args.output.exists());
    args.expected_prior_sha256 = hash;
    args.candidate_config = args.prior_config.parent().unwrap().join("candidate.conf");
    assert!(stage(&args, &json!({"configuration":"fsync = 'on'\n"})).is_err());
    assert!(!args.candidate_config.exists());
    Ok(())
}

#[test]
fn given_unknown_outcome_with_exact_candidate_when_retried_then_only_missing_receipt_is_completed()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let args = fixture(root.path())?;
    let prepared = json!({"configuration":"fsync = 'on'\n"});
    stage(&args, &prepared)?;
    let prior = fs::read(&args.output)?;
    fs::remove_file(&args.output)?;
    stage(&args, &prepared)?;
    assert_eq!(fs::read(&args.output)?, prior);
    fs::remove_file(&args.output)?;
    fs::write(&args.candidate_config, b"different")?;
    assert!(stage(&args, &prepared).is_err());
    assert_eq!(fs::read(&args.candidate_config)?, b"different");
    assert!(!args.output.exists());
    Ok(())
}
