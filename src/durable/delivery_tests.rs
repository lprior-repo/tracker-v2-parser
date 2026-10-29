use super::*;
use crate::durable::operations::{Artifact, PublishedExport};
use sha2::{Digest, Sha256};

fn generation(root: &Path, number: u8) -> Result<PublishedExport> {
    let workbook = root.join(format!("generation-{number}.xlsx"));
    fs::write(&workbook, [number])?;
    let manifest = root.join(format!("generation-{number}.json"));
    fs::write(&manifest, format!("{{\"generation\":{number}}}"))?;
    Ok(PublishedExport {
        binding: number.to_string(),
        workbook: Artifact {
            path: workbook.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&workbook)?)),
            bytes: 1,
        },
        manifest: Artifact {
            path: manifest.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&manifest)?)),
            bytes: fs::metadata(manifest)?.len(),
        },
        canonical_pointer: root.join("current-export.json"),
        promoted: true,
    })
}

#[test]
fn given_a_verified_generation_when_delivered_twice_then_the_existing_workbook_changes_atomically_and_stays_readable()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let output = root.path().join("athletes.xlsx");
    fs::write(&output, b"previous verified workbook")?;
    let current = generation(root.path(), 1)?;
    fs::write(&current.canonical_pointer, serde_json::to_vec(&current)?)?;
    assert!(publish(&output, &current)?);
    assert!(fs::symlink_metadata(&output)?.file_type().is_file());
    assert_eq!(fs::read(&output)?, [1]);
    fs::write(&output, b"user edits only the delivered copy")?;
    assert_eq!(fs::read(&current.workbook.path)?, [1]);
    assert!(publish(&output, &current)?);
    assert_eq!(fs::read(&output)?, fs::read(&current.workbook.path)?);
    assert_eq!(
        fs::read(output.with_extension("manifest.json"))?,
        fs::read(current.manifest.path)?
    );
    Ok(())
}

#[test]
fn given_a_newer_publication_when_an_old_delivery_replays_then_the_newer_workbook_is_preserved()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let output = root.path().join("athletes.xlsx");
    let old = generation(root.path(), 1)?;
    let current = generation(root.path(), 2)?;
    fs::write(&current.canonical_pointer, serde_json::to_vec(&current)?)?;
    assert!(publish(&output, &current)?);
    assert!(!publish(&output, &old)?);
    assert_eq!(fs::read(&output)?, [2]);
    assert_eq!(fs::read(&old.workbook.path)?, [1]);
    Ok(())
}

#[test]
fn large_workbook_delivery_streams_verified_copies_and_retains_last_good_on_corruption()
-> Result<()> {
    use crate::{
        config::Config,
        durable::operations::{OperationSpec, Workspace},
    };
    use std::io::{Seek, SeekFrom, Write};
    let root = tempfile::tempdir()?;
    let workspace = Workspace::open(
        root.path(),
        "large-delivery",
        &OperationSpec::Export {
            config: Config::default(),
        },
    )?;
    let source = workspace.path("large.xlsx")?;
    File::create(&source)?.set_len(64 * 1024 * 1024 + 4096)?;
    let mut current = generation(root.path(), 1)?;
    current.workbook = workspace.inspect_workbook("large.xlsx")?;
    fs::write(&current.canonical_pointer, serde_json::to_vec(&current)?)?;
    let output = root.path().join("delivered.xlsx");
    assert!(publish(&output, &current)?);
    let delivered = Artifact {
        path: output.clone(),
        ..current.workbook.clone()
    };
    workspace.verify_workbook(&delivered)?;
    assert!(fs::symlink_metadata(&output)?.file_type().is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(fs::metadata(&output)?.ino(), fs::metadata(&source)?.ino());
    }
    let mut file = File::options().write(true).open(&source)?;
    file.seek(SeekFrom::Start(current.workbook.bytes - 1))?;
    file.write_all(&[1])?;
    assert!(publish(&output, &current).is_err());
    workspace.verify_workbook(&delivered)?;
    file.set_len(current.workbook.bytes - 1)?;
    assert!(publish(&output, &current).is_err());
    workspace.verify_workbook(&delivered)?;
    assert!(
        !fs::read_dir(root.path())?.any(|entry| entry.is_ok_and(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".durable-delivery-")))
    );
    Ok(())
}

#[test]
fn delivery_rejects_outside_root_and_symlink_artifacts_without_replacing_the_last_copy()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let output = root.path().join("athletes.xlsx");
    fs::write(&output, b"last verified copy")?;
    let mut current = generation(root.path(), 1)?;
    let external = tempfile::NamedTempFile::new()?;
    fs::write(external.path(), [1])?;
    current.workbook.path = external.path().into();
    fs::write(&current.canonical_pointer, serde_json::to_vec(&current)?)?;
    assert!(publish(&output, &current).is_err());
    assert_eq!(fs::read(&output)?, b"last verified copy");
    #[cfg(unix)]
    {
        current = generation(root.path(), 2)?;
        let linked = root.path().join("linked.xlsx");
        std::os::unix::fs::symlink(&current.workbook.path, &linked)?;
        current.workbook.path = linked;
        fs::write(&current.canonical_pointer, serde_json::to_vec(&current)?)?;
        assert!(publish(&output, &current).is_err());
        assert_eq!(fs::read(&output)?, b"last verified copy");
    }
    assert!(
        !fs::read_dir(root.path())?.any(|entry| entry.is_ok_and(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".durable-delivery-")))
    );
    Ok(())
}
