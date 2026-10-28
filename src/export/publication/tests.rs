use super::{claim_published, finish_claim, rollback};
use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn published() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
    let directory = tempfile::tempdir()?;
    let staged = directory.path().join("staged.xlsx");
    let destination = directory.path().join("published.xlsx");
    fs::write(&staged, b"own verified workbook")?;
    fs::hard_link(&staged, &destination)?;
    Ok((directory, staged, destination))
}

#[test]
fn given_own_entry_claimed_when_another_writer_recreates_destination_then_only_claimed_entry_is_deleted()
-> Result<()> {
    let (directory, staged, destination) = published()?;
    let claim = claim_published(&staged, &destination)?.context("claimed published entry")?;
    let recovery = claim.directory.clone();
    fs::write(&destination, b"concurrent replacement")?;
    finish_claim(&claim)?;
    assert_eq!(fs::read(&destination)?, b"concurrent replacement");
    assert_eq!(fs::read(&staged)?, b"own verified workbook");
    assert!(!recovery.exists());
    assert_eq!(fs::read_dir(directory.path())?.count(), 2);
    Ok(())
}

#[test]
fn given_changed_published_file_when_rolling_back_then_it_is_restored_and_other_own_parts_are_removed()
-> Result<()> {
    let (directory, staged, destination) = published()?;
    let replacement = directory.path().join("replacement.xlsx");
    fs::write(&replacement, b"unrelated replacement")?;
    fs::rename(&replacement, &destination)?;
    let other_staged = directory.path().join("other-staged.xlsx");
    let other_destination = directory.path().join("other-published.xlsx");
    fs::write(&other_staged, b"other own workbook")?;
    fs::hard_link(&other_staged, &other_destination)?;
    let error = rollback(&[(&other_staged, &other_destination), (&staged, &destination)])
        .err()
        .context("changed entry reports rollback conflict")?;
    assert!(format!("{error:#}").contains("restored without overwrite"));
    assert_eq!(fs::read(&destination)?, b"unrelated replacement");
    assert_eq!(fs::read(&staged)?, b"own verified workbook");
    assert!(!other_destination.exists());
    assert_eq!(fs::read(&other_staged)?, b"other own workbook");
    assert_eq!(fs::read_dir(directory.path())?.count(), 3);
    Ok(())
}

#[test]
fn given_unrelated_symlink_when_claimed_during_rollback_then_link_and_target_are_preserved()
-> Result<()> {
    let (directory, staged, destination) = published()?;
    let target = directory.path().join("unrelated.txt");
    fs::write(&target, b"unrelated target")?;
    fs::remove_file(&destination)?;
    symlink(&target, &destination)?;
    let claim = claim_published(&staged, &destination)?.context("claimed symlink")?;
    let recovery = claim.directory.clone();
    let error = finish_claim(&claim)
        .err()
        .context("symlink identity mismatch")?;
    assert!(format!("{error:#}").contains("restored without overwrite"));
    assert!(fs::symlink_metadata(&destination)?.file_type().is_symlink());
    assert_eq!(fs::read_link(&destination)?, target);
    assert_eq!(fs::read(&destination)?, b"unrelated target");
    assert!(!recovery.exists());
    Ok(())
}

#[test]
fn given_unrelated_nonempty_directory_when_claimed_during_rollback_then_its_contents_are_restored()
-> Result<()> {
    let (_directory, staged, destination) = published()?;
    fs::remove_file(&destination)?;
    fs::create_dir(&destination)?;
    fs::write(
        destination.join("important.txt"),
        b"unrelated directory data",
    )?;
    let claim = claim_published(&staged, &destination)?.context("claimed directory")?;
    let recovery = claim.directory.clone();
    let error = finish_claim(&claim)
        .err()
        .context("directory identity mismatch")?;
    assert!(format!("{error:#}").contains("restored without overwrite"));
    assert!(fs::symlink_metadata(&destination)?.is_dir());
    assert_eq!(
        fs::read(destination.join("important.txt"))?,
        b"unrelated directory data"
    );
    assert!(!recovery.exists());
    Ok(())
}

#[test]
fn given_unrelated_claim_and_new_destination_when_restore_conflicts_then_both_entries_survive_with_recovery_path()
-> Result<()> {
    for replacement in ["file", "symlink", "directory"] {
        let (directory, staged, destination) = published()?;
        let unrelated = directory.path().join("unrelated.xlsx");
        fs::write(&unrelated, b"captured unrelated data")?;
        fs::rename(&unrelated, &destination)?;
        let claim = claim_published(&staged, &destination)?.context("claimed unrelated entry")?;
        let recovery = claim.directory.clone();
        let retained = claim.entry.clone();
        match replacement {
            "file" => fs::write(&destination, b"newest file")?,
            "symlink" => symlink("missing-target", &destination)?,
            "directory" => fs::create_dir(&destination)?,
            other => anyhow::bail!("unexpected replacement fixture {other}"),
        }
        let error = finish_claim(&claim)
            .err()
            .context("restoration must not overwrite")?;
        assert!(format!("{error:#}").contains(&retained.display().to_string()));
        assert!(format!("{error:#}").contains("retained for recovery"));
        assert!(recovery.is_dir());
        assert_eq!(fs::read(&retained)?, b"captured unrelated data");
        match replacement {
            "file" => assert_eq!(fs::read(&destination)?, b"newest file"),
            "symlink" => assert_eq!(
                fs::read_link(&destination)?,
                PathBuf::from("missing-target")
            ),
            "directory" => assert!(fs::symlink_metadata(&destination)?.is_dir()),
            other => anyhow::bail!("unexpected replacement fixture {other}"),
        }
        assert_eq!(fs::read(&staged)?, b"own verified workbook");
    }
    Ok(())
}

#[test]
fn given_published_destination_already_removed_when_rolling_back_then_no_claim_directory_is_left()
-> Result<()> {
    let (directory, staged, destination) = published()?;
    fs::remove_file(&destination)?;
    rollback(&[(&staged, &destination)])?;
    assert_eq!(fs::read(&staged)?, b"own verified workbook");
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}
