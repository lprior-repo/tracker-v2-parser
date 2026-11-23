use super::*;
use anyhow::Result;
use sha2::{Digest, Sha256};

fn body() -> Vec<u8> {
    b"immutable sanitized source capture and parsed payload\n".repeat(8192)
}

#[tokio::test]
async fn given_new_large_blob_when_published_then_ref_and_logical_bytes_are_identical() -> Result<()>
{
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let bytes = body();
    let expected = ArtifactRef {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes: u64::try_from(bytes.len())?,
    };
    let reference = super::super::write(
        crate::durable::page::PageFiles::new(root.path(), &blocking),
        bytes.clone(),
        bytes.len(),
    )
    .await?;
    assert_eq!(reference, expected);
    assert_eq!(
        super::super::read(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            &reference,
            bytes.len()
        )
        .await?,
        bytes
    );
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        let metadata = fs::metadata(root.path().join(format!("{}.blob", reference.sha256)))?;
        assert_ne!(metadata.st_flags() & 0x20, 0);
        assert!(metadata.st_blocks() * 512 < reference.bytes);
    }
    Ok(())
}

#[tokio::test]
async fn given_same_hash_concurrent_writers_when_published_then_one_complete_blob_survives()
-> Result<()> {
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let bytes = body();
    let (left, right) = tokio::join!(
        super::super::write(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            bytes.clone(),
            bytes.len()
        ),
        super::super::write(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            bytes.clone(),
            bytes.len()
        )
    );
    let left = left?;
    assert_eq!(left, right?);
    assert_eq!(
        super::super::read(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            &left,
            bytes.len()
        )
        .await?,
        bytes
    );
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn given_existing_raw_blob_when_written_again_then_inode_and_bytes_remain_untouched()
-> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let bytes = body();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let path = root.path().join(format!("{hash}.blob"));
    fs::write(&path, &bytes)?;
    let inode = fs::metadata(&path)?.ino();
    let reference = super::super::write(
        crate::durable::page::PageFiles::new(root.path(), &blocking),
        bytes.clone(),
        bytes.len(),
    )
    .await?;
    assert_eq!(fs::metadata(&path)?.ino(), inode);
    assert_eq!(fs::read(&path)?, bytes);
    assert_eq!(
        super::super::read(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            &reference,
            bytes.len()
        )
        .await?,
        bytes
    );
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn given_corrupt_existing_digest_when_written_then_failure_preserves_existing_file()
-> Result<()> {
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let bytes = body();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let path = root.path().join(format!("{hash}.blob"));
    fs::write(&path, b"corrupt existing checkpoint")?;
    assert!(
        super::super::write(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            bytes.clone(),
            bytes.len()
        )
        .await
        .is_err()
    );
    assert_eq!(fs::read(&path)?, b"corrupt existing checkpoint");
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn given_small_blob_when_stored_then_regular_raw_publication_is_preserved() -> Result<()> {
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let bytes = b"small immutable fact".to_vec();
    let reference = super::super::write(
        crate::durable::page::PageFiles::new(root.path(), &blocking),
        bytes.clone(),
        bytes.len(),
    )
    .await?;
    assert_eq!(
        super::super::read(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            &reference,
            bytes.len()
        )
        .await?,
        bytes
    );
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn given_faulty_or_corrupt_compressor_when_staging_then_raw_bytes_survive_and_stage_is_cleaned()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let bytes = body();
    let reference = ArtifactRef {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes: u64::try_from(bytes.len())?,
    };
    let raw = root.path().join("raw.tmp");
    fs::write(&raw, &bytes)?;
    fn corrupt(_: &Path, destination: &Path) -> Result<()> {
        fs::write(destination, b"wrong staged bytes")?;
        Ok(())
    }
    fn failed(_: &Path, _: &Path) -> Result<()> {
        anyhow::bail!("owned compression fault")
    }
    for copy_body in [corrupt as fn(&Path, &Path) -> Result<()>, failed] {
        assert!(prepare_with(root.path(), &raw, &reference, &bytes, copy_body).is_err());
        assert_eq!(fs::read(&raw)?, bytes);
        assert_eq!(fs::read_dir(root.path())?.count(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn given_preexisting_symlink_when_publishing_then_target_and_link_are_preserved() -> Result<()>
{
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let bytes = body();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let target = root.path().join("unrelated.data");
    fs::write(&target, b"preserve unrelated file")?;
    let path = root.path().join(format!("{hash}.blob"));
    std::os::unix::fs::symlink(&target, &path)?;
    assert!(
        super::super::write(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            bytes.clone(),
            bytes.len()
        )
        .await
        .is_err()
    );
    assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
    assert_eq!(fs::read(&target)?, b"preserve unrelated file");
    assert_eq!(fs::read_dir(root.path())?.count(), 2);
    Ok(())
}

#[tokio::test]
async fn given_incompressible_body_when_stored_then_raw_fallback_has_identical_bytes() -> Result<()>
{
    let blocking = crate::durable::blocking::BlockingStages::new();
    let root = tempfile::tempdir()?;
    let mut state = 0x9e3779b97f4a7c15u64;
    let bytes = (0..262144)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect::<Vec<_>>();
    let reference = super::super::write(
        crate::durable::page::PageFiles::new(root.path(), &blocking),
        bytes.clone(),
        bytes.len(),
    )
    .await?;
    assert_eq!(
        super::super::read(
            crate::durable::page::PageFiles::new(root.path(), &blocking),
            &reference,
            bytes.len()
        )
        .await?,
        bytes
    );
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        assert_eq!(
            fs::metadata(root.path().join(format!("{}.blob", reference.sha256)))?.st_flags() & 0x20,
            0
        );
    }
    Ok(())
}
