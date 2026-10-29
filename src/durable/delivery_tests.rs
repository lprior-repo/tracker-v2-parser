use super::*;
use crate::durable::operations::{Artifact, PublishedExport};

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
