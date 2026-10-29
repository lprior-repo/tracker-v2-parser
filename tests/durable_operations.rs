use anyhow::Result;
use track_scrapper_v2::{
    config::Config,
    durable::operations::{Artifact, OperationSpec, Workspace},
};

#[test]
fn operation_identity_rejects_changed_inputs_and_relative_roots() -> Result<()> {
    let root = tempfile::tempdir()?;
    let spec = OperationSpec::Export {
        config: Config::default(),
    };
    let workspace = Workspace::open(root.path(), "export-a", &spec)?;
    let replay = Workspace::open(root.path(), "export-a", &spec)?;
    assert_eq!(workspace.binding(), replay.binding());
    let mut changed = Config::default();
    changed.states = vec!["WI".into()];
    let error = Workspace::open(
        root.path(),
        "export-a",
        &OperationSpec::Export { config: changed },
    )
    .err()
    .expect("changed input must fail");
    assert!(error.to_string().contains("binding"));
    assert!(Workspace::open(std::path::Path::new("relative"), "a", &spec).is_err());
    Ok(())
}

#[test]
fn artifacts_verify_hash_bounds_and_canonical_root_ownership() -> Result<()> {
    let root = tempfile::tempdir()?;
    let spec = OperationSpec::Export {
        config: Config::default(),
    };
    let workspace = Workspace::open(root.path(), "export-a", &spec)?;
    let artifact = workspace.store("input.json", b"source evidence")?;
    assert_eq!(workspace.read(&artifact)?, b"source evidence");
    std::fs::write(&artifact.path, b"changed evidence")?;
    assert!(
        workspace
            .read(&artifact)
            .err()
            .expect("tamper must fail")
            .to_string()
            .contains("hash")
    );
    let external = tempfile::NamedTempFile::new()?;
    let outside = Artifact {
        path: external.path().to_path_buf(),
        sha256: "0".repeat(64),
        bytes: 0,
    };
    assert!(workspace.read(&outside).is_err());
    assert!(workspace.store("../escape", b"bad").is_err());
    Ok(())
}
