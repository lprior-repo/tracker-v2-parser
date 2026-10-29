use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};

use super::super::artifacts::digest;
use super::{PublishedExport, VerifiedExport, Workspace};

#[derive(serde::Serialize, serde::Deserialize)]
struct Intent {
    previous_sha256: Option<String>,
}

fn pointer_digest(path: &Path) -> Result<Option<String>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(1_048_577).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "canonical pointer exceeds 1 MiB");
    Ok(Some(digest(&bytes)))
}

pub fn publish_export(workspace: &Workspace, verified: &VerifiedExport) -> Result<PublishedExport> {
    ensure!(
        verified.binding == workspace.binding(),
        "export publication binding mismatch"
    );
    workspace.read(&verified.workbook)?;
    workspace.read(&verified.frozen_manifest)?;
    let original: VerifiedExport = workspace
        .load("verification-receipt.json")?
        .context("export verification receipt absent")?;
    ensure!(
        serde_json::to_vec(&original)? == serde_json::to_vec(verified)?,
        "publication must use verified snapshot receipt"
    );
    if let Some(receipt) = workspace.load::<PublishedExport>("publication-receipt.json")? {
        ensure!(
            receipt.binding == workspace.binding() && receipt.workbook == verified.workbook,
            "publication receipt binding mismatch"
        );
        workspace.read(&receipt.manifest)?;
        return Ok(receipt);
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(workspace.root().join("canonical-publication.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    let manifest = workspace.record("published-export.json", verified)?;
    let pointer = workspace.root().join("current-export.json");
    let intent = if let Some(intent) = workspace.load::<Intent>("publication-intent.json")? {
        intent
    } else {
        let intent = Intent {
            previous_sha256: pointer_digest(&pointer)?,
        };
        workspace.record("publication-intent.json", &intent)?;
        intent
    };
    let mut receipt = PublishedExport {
        binding: workspace.binding().into(),
        workbook: verified.workbook.clone(),
        manifest,
        canonical_pointer: pointer.clone(),
        promoted: true,
    };
    let bytes = serde_json::to_vec(&receipt)?;
    let current = pointer_digest(&pointer)?;
    if current != intent.previous_sha256 && current.as_deref() != Some(digest(&bytes).as_str()) {
        receipt.promoted = false;
        workspace.record("publication-receipt.json", &receipt)?;
        return Ok(receipt);
    }
    let prepared = workspace.path("canonical-pointer.tmp")?;
    if prepared.try_exists()? {
        fs::remove_file(&prepared).context("removing interrupted owned canonical pointer")?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&prepared)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&prepared, &pointer)
        .context("atomically publishing verified canonical export pointer")?;
    File::open(workspace.root())?.sync_all()?;
    workspace.record("publication-receipt.json", &receipt)?;
    Ok(receipt)
}
