use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use super::super::artifacts::digest;
use super::{FrozenExport, PublishedExport, VerifiedExport, Workspace, frozen_plan};

#[derive(serde::Serialize, serde::Deserialize)]
struct Intent {
    previous_sha256: Option<String>,
}

#[derive(serde::Serialize)]
struct NamedScope {
    name: String,
    states: Vec<String>,
    graduation_years: Vec<i32>,
}

fn destination(
    workspace: &Workspace,
    verified: &VerifiedExport,
) -> Result<(PathBuf, Option<NamedScope>)> {
    let plan = frozen_plan(
        workspace,
        &FrozenExport {
            manifest: verified.frozen_manifest.clone(),
        },
    )?;
    let scope = plan.config.export.validated_name()?.map(|name| NamedScope {
        name: name.to_owned(),
        states: plan.config.states.clone(),
        graduation_years: plan.config.graduation_years.clone(),
    });
    let name = scope.as_ref().map_or_else(
        || "current-export.json".to_owned(),
        |scope| format!("current-named-{}.json", scope.name),
    );
    Ok((workspace.root().join(name), scope))
}

fn claim_scope(workspace: &Workspace, scope: &NamedScope) -> Result<()> {
    let path = workspace
        .root()
        .join(format!("publication-named-{}.scope.json", scope.name));
    let artifact = workspace.record("publication-scope.json", scope)?;
    match fs::hard_link(&artifact.path, &path) {
        Ok(()) => File::open(workspace.root())?
            .sync_all()
            .context("syncing named export scope"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 1_048_576,
                "named export scope owner must be a bounded regular file"
            );
            let mut bytes = Vec::new();
            File::open(&path)?.take(1_048_577).read_to_end(&mut bytes)?;
            ensure!(
                bytes == serde_json::to_vec(scope)?,
                "export name already belongs to a different state or graduation-year scope"
            );
            Ok(())
        }
        Err(error) => Err(error).context("claiming named export scope"),
    }
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

fn verify_publication(workspace: &Workspace, verified: &VerifiedExport) -> Result<()> {
    ensure!(
        verified.binding == workspace.binding(),
        "export publication binding mismatch"
    );
    workspace.verify_workbook(&verified.workbook)?;
    workspace.read(&verified.frozen_manifest)?;
    let original: VerifiedExport = workspace
        .load("verification-receipt.json")?
        .context("export verification receipt absent")?;
    ensure!(
        serde_json::to_vec(&original)? == serde_json::to_vec(verified)?,
        "publication must use verified snapshot receipt"
    );
    Ok(())
}

fn commit_pointer(workspace: &Workspace, pointer: &Path, bytes: &[u8]) -> Result<()> {
    let prepared = workspace.path("canonical-pointer.tmp")?;
    if prepared.try_exists()? {
        fs::remove_file(&prepared).context("removing interrupted owned canonical pointer")?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&prepared)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&prepared, pointer)
        .context("atomically publishing verified canonical export pointer")?;
    File::open(workspace.root())?.sync_all()?;
    Ok(())
}

pub fn publish_export(workspace: &Workspace, verified: &VerifiedExport) -> Result<PublishedExport> {
    verify_publication(workspace, verified)?;
    let (pointer, scope) = destination(workspace, verified)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(workspace.root().join("canonical-publication.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    if let Some(scope) = &scope {
        claim_scope(workspace, scope)?;
    }
    if let Some(receipt) = workspace.load::<PublishedExport>("publication-receipt.json")? {
        ensure!(
            receipt.binding == workspace.binding()
                && receipt.workbook == verified.workbook
                && receipt.canonical_pointer == pointer,
            "publication receipt binding mismatch"
        );
        workspace.read(&receipt.manifest)?;
        return Ok(receipt);
    }
    let manifest = workspace.record("published-export.json", verified)?;
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
    commit_pointer(workspace, &pointer, &bytes)?;
    workspace.record("publication-receipt.json", &receipt)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests;
