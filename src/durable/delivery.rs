use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};

use super::operations::{
    Artifact, FrozenExport, PublishedExport, VerifiedExport, Workspace, frozen_plan,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn publish_configured(
    path: &Path,
    workspace: &Workspace,
    export: &PublishedExport,
) -> Result<bool> {
    let verified: VerifiedExport = serde_json::from_slice(&workspace.read(&export.manifest)?)?;
    ensure!(
        verified.binding == workspace.binding()
            && verified.binding == export.binding
            && verified.workbook == export.workbook,
        "delivery publication binding mismatch"
    );
    let plan = frozen_plan(
        workspace,
        &FrozenExport {
            manifest: verified.frozen_manifest,
        },
    )?;
    let name = plan.config.export.validated_name()?;
    let pointer = name.map_or_else(
        || "current-export.json".to_owned(),
        |name| format!("current-named-{name}.json"),
    );
    ensure!(
        export.canonical_pointer == workspace.root().join(pointer),
        "delivery publication scope mismatch"
    );
    let destination = if let Some(name) = name {
        ensure!(
            !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.eq_ignore_ascii_case(name)),
            "named export delivery must not replace the legacy workbook"
        );
        path.with_file_name(format!("{name}.xlsx"))
    } else {
        path.to_path_buf()
    };
    publish(&destination, export)
}

pub(super) fn publish(path: &Path, export: &PublishedExport) -> Result<bool> {
    ensure!(
        path.is_absolute(),
        "published workbook path must be absolute"
    );
    ensure!(
        path.extension()
            .is_some_and(|extension| extension == "xlsx"),
        "published workbook must have an xlsx extension"
    );
    let root = export
        .canonical_pointer
        .parent()
        .context("canonical export parent")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("canonical-publication.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    let mut bytes = Vec::new();
    File::open(&export.canonical_pointer)?
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "canonical pointer exceeds 1 MiB");
    let current: PublishedExport = serde_json::from_slice(&bytes)?;
    if !export.promoted || current.binding != export.binding || current.workbook != export.workbook
    {
        return Ok(false);
    }
    let directory = path.parent().context("published workbook parent")?;
    ensure!(directory.is_dir(), "published workbook parent must exist");
    export
        .manifest
        .stream_to(root, 64 * 1024 * 1024, &mut std::io::sink())?;
    replace_file(
        path,
        &export.workbook,
        root,
        crate::export::MAX_EXPORT_BYTES,
    )?;
    replace_file(
        &path.with_extension("manifest.json"),
        &export.manifest,
        root,
        64 * 1024 * 1024,
    )?;
    File::open(directory)?.sync_all()?;
    Ok(true)
}

fn replace_file(destination: &Path, source: &Artifact, root: &Path, limit: u64) -> Result<()> {
    let destination_entry = destination
        .parent()
        .context("delivery parent absent")?
        .canonicalize()?
        .join(
            destination
                .file_name()
                .context("delivery file name absent")?,
        );
    ensure!(
        destination_entry != source.path.canonicalize()?,
        "delivery must not replace the immutable artifact"
    );
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_file_name(format!(
        ".durable-delivery-{}-{sequence}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .context("staging verified workbook delivery")?;
    let result = (|| -> Result<()> {
        source.stream_to(root, limit, &mut file)?;
        file.sync_all()?;
        fs::rename(&temporary, destination).context("publishing verified workbook delivery")?;
        Ok(())
    })();
    drop(file);
    if result.is_err() {
        fs::remove_file(&temporary).context("removing failed workbook delivery staging file")?;
    }
    result
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
