use super::{cleanup_staged, publication};
use anyhow::Result;
use std::fs;

#[test]
fn concurrent_destination_preserves_other_file_and_removes_own_published_links() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let staged = [
        directory.path().join("stage-1"),
        directory.path().join("stage-2"),
    ];
    let destinations = [
        directory.path().join("part-1"),
        directory.path().join("part-2"),
    ];
    fs::write(&staged[0], b"first workbook")?;
    fs::write(&staged[1], b"second workbook")?;
    fs::write(&destinations[1], b"other user's file")?;

    let error = publication::publish(&staged, &destinations, None)
        .err()
        .ok_or_else(|| anyhow::anyhow!("publication unexpectedly succeeded"))?;
    assert!(error.to_string().contains("publication incomplete"));
    assert!(!destinations[0].exists());
    assert_eq!(fs::read(&destinations[1])?, b"other user's file");
    cleanup_staged(&staged)?;
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}
