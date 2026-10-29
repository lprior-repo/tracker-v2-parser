use std::{
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
};

use anyhow::Result;
use track_scrapper_v2::{
    config::Config,
    durable::operations::{OperationSpec, Workspace},
    export::MAX_EXPORT_BYTES,
};

#[test]
fn streamed_workbook_artifact_exceeds_old_limit_without_relaxing_json_input_bounds() -> Result<()> {
    let root = tempfile::tempdir()?;
    let workspace = Workspace::open(
        root.path(),
        "large-workbook",
        &OperationSpec::Export {
            config: Config::default(),
        },
    )?;
    let path = workspace.path("large.xlsx")?;
    File::create(&path)?.set_len(64 * 1024 * 1024 + 4096)?;
    let artifact = workspace.inspect_workbook("large.xlsx")?;
    assert_eq!(artifact.bytes, 64 * 1024 * 1024 + 4096);
    workspace.verify_workbook(&artifact)?;
    assert!(
        workspace.read(&artifact).is_err(),
        "ordinary in-memory reads retain their 64 MiB limit"
    );
    assert!(
        workspace.inspect("large.xlsx").is_err(),
        "generic inspection retains its 64 MiB limit"
    );
    let result = workspace.complete(&serde_json::json!({"rows": 20_000}), Some(&artifact))?;
    assert_eq!(result.output.as_ref(), Some(&path));
    assert!(result.receipt.bytes < 1024 * 1024);
    assert_eq!(MAX_EXPORT_BYTES, 8_589_934_592_u64);
    let mut over = artifact.clone();
    over.bytes = MAX_EXPORT_BYTES + 1;
    assert!(workspace.verify_workbook(&over).is_err());
    let mut wrong_digest = artifact.clone();
    wrong_digest.sha256 = "0".repeat(64);
    assert!(workspace.verify_workbook(&wrong_digest).is_err());
    let mut file = File::options().write(true).open(&path)?;
    file.seek(SeekFrom::Start(artifact.bytes - 1))?;
    file.write_all(&[1])?;
    assert!(
        workspace.verify_workbook(&artifact).is_err(),
        "same-length corruption is rejected"
    );
    file.set_len(artifact.bytes - 1)?;
    assert!(
        workspace.verify_workbook(&artifact).is_err(),
        "truncation is rejected"
    );
    Ok(())
}

#[test]
fn streamed_workbooks_reject_foreign_roots_symlinks_and_non_workbook_extensions() -> Result<()> {
    let root = tempfile::tempdir()?;
    let workspace = Workspace::open(
        root.path(),
        "path-boundary",
        &OperationSpec::Export {
            config: Config::default(),
        },
    )?;
    fs::write(workspace.path("owned.xlsx")?, b"fixture workbook bytes")?;
    let artifact = workspace.inspect_workbook("owned.xlsx")?;
    let outside = tempfile::tempdir()?;
    let outside_path = outside.path().join("outside.xlsx");
    fs::copy(&artifact.path, &outside_path)?;
    let mut foreign = artifact.clone();
    foreign.path = outside_path;
    assert!(workspace.verify_workbook(&foreign).is_err());
    assert!(
        workspace
            .complete(&serde_json::json!({}), Some(&foreign))
            .is_err()
    );
    assert!(!workspace.path("operation-complete.json")?.exists());
    fs::copy(&artifact.path, workspace.path("not-workbook.json")?)?;
    assert!(workspace.inspect_workbook("not-workbook.json").is_err());
    #[cfg(unix)]
    {
        let linked = workspace.path("linked.xlsx")?;
        std::os::unix::fs::symlink(&artifact.path, &linked)?;
        let mut link = artifact.clone();
        link.path = linked;
        assert!(workspace.verify_workbook(&link).is_err());
        assert!(workspace.inspect_workbook("linked.xlsx").is_err());
    }
    Ok(())
}
