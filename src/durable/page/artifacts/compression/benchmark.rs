use super::*;
use sha2::{Digest, Sha256};
use std::{os::macos::fs::MetadataExt, time::Instant};

#[test]
fn given_representative_capture_bodies_when_staged_then_identity_and_allocation_are_measured()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let source = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/milesplit/athlete-oh-15005189.html"
    ));
    let mut observations = Vec::new();
    for nonce in 0..24 {
        let mut bytes = source.to_vec();
        bytes.extend_from_slice(format!("storage measurement {nonce}").as_bytes());
        let reference = ArtifactRef {
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: u64::try_from(bytes.len())?,
        };
        let raw = root.path().join(format!("{nonce}.tmp"));
        fs::write(&raw, &bytes)?;
        File::open(&raw)?.sync_all()?;
        let before = fs::metadata(&raw)?.st_blocks() * 512;
        let started = Instant::now();
        let stage = prepare_with(root.path(), &raw, &reference, &bytes, copy)?
            .ok_or_else(|| anyhow::anyhow!("representative capture did not compress"))?;
        let elapsed = started.elapsed().as_micros();
        let allocated = fs::metadata(stage.path())?.st_blocks() * 512;
        assert!(allocated < before);
        assert_eq!(
            super::super::read_sync(&stage.directory, &reference, bytes.len())?,
            bytes
        );
        observations.push((elapsed, before, allocated));
        drop(stage);
        fs::remove_file(raw)?;
    }
    let mut latencies = observations.iter().map(|row| row.0).collect::<Vec<_>>();
    latencies.sort_unstable();
    println!(
        "STORAGE_COMPRESSION_MEASUREMENT {}",
        serde_json::json!({"samples":observations.len(),"logical_capture_bytes":source.len(),"median_us":latencies[12],"p95_us":latencies[22],"max_us":latencies[23],"raw_allocated_bytes":observations.iter().map(|row|row.1).sum::<u64>(),"compressed_allocated_bytes":observations.iter().map(|row|row.2).sum::<u64>(),"logical_identity_verified":true})
    );
    assert_eq!(fs::read_dir(root.path())?.count(), 0);
    Ok(())
}
