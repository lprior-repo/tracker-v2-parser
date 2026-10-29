mod read;

use std::{collections::BTreeSet, io::Read, path::Path};

use anyhow::{Context, Result};
use flate2::{Compression, GzBuilder};

use super::{
    files,
    manifest::{Fingerprint, Inventory, Manifest},
};

pub(super) use read::unpack;

pub(super) fn write(root: &Path, destination: &Path) -> Result<()> {
    let files = files::list(root)?;
    let directories = files
        .iter()
        .flat_map(|path| path.ancestors().skip(1))
        .filter(|path| *path != root && path.starts_with(root))
        .map(Path::to_owned)
        .chain(["evidence", "exports", "durable-artifacts"].map(|name| root.join(name)))
        .filter(|path| path.is_dir())
        .collect::<BTreeSet<_>>();
    let output = files::create(destination)?;
    let compressed = GzBuilder::new()
        .mtime(0)
        .write(output, Compression::default());
    let mut archive = tar::Builder::new(compressed);
    directories
        .iter()
        .chain(files.iter())
        .try_for_each(|path| {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("archive filename is not UTF-8")?;
            let metadata = std::fs::symlink_metadata(path)?;
            let mut header = tar::Header::new_ustar();
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_mode(if metadata.is_dir() { 0o700 } else { 0o600 });
            header.set_entry_type(if metadata.is_dir() {
                tar::EntryType::Directory
            } else {
                tar::EntryType::Regular
            });
            header.set_size(if metadata.is_dir() { 0 } else { metadata.len() });
            if header.set_path(relative).is_err() {
                archive.append_pax_extensions(Some(("path", relative.as_bytes())))?;
                header.set_path("payload")?;
            }
            header.set_cksum();
            if metadata.is_dir() {
                archive.append(&header, std::io::empty())?;
            } else {
                archive.append(&header, files::open(path)?)?;
            }
            Ok::<_, anyhow::Error>(())
        })?;
    archive.finish()?;
    archive.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

pub(super) fn validate(root: &Path, mut actual: Vec<Fingerprint>) -> Result<(Manifest, Inventory)> {
    actual.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest: Manifest = files::load(&root.join("manifest.json"), 64 * 1024 * 1024)?;
    let inventory: Inventory = files::load(&root.join("inventory.json"), super::manifest::SMALL)?;
    manifest.validate(&actual, &inventory)?;
    super::references::validate(root, &manifest.required_artifacts, &actual)?;
    super::paths::config(&root.join("config.toml"))?;
    if root.join("scrape.toml").try_exists()? {
        super::paths::config(&root.join("scrape.toml"))?;
    }
    let mut magic = [0_u8; 5];
    files::open(&root.join("database.dump"))?.read_exact(&mut magic)?;
    anyhow::ensure!(
        &magic == b"PGDMP",
        "database payload is not a PostgreSQL custom dump"
    );
    anyhow::ensure!(
        root.join("evidence").is_dir() && root.join("exports").is_dir(),
        "archive directories are missing"
    );
    if manifest.artifacts_dir.is_some() {
        anyhow::ensure!(
            root.join("durable-artifacts").is_dir(),
            "durable artifact directory is missing"
        );
    }
    super::exports::validate(&root.join("exports"))?;
    Ok((manifest, inventory))
}
