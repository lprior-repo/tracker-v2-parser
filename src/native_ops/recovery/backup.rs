use std::{
    collections::VecDeque,
    fs::{self, File},
    io::Read,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::io;

const MAX_ENTRIES: usize = 20_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Inventory {
    directories: Vec<PathBuf>,
    files: Vec<Entry>,
    bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

fn inventory(root: &Path) -> Result<Inventory> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "backup root must be a real directory"
    );
    let mut pending = VecDeque::from([PathBuf::new()]);
    let mut result = Inventory {
        directories: Vec::new(),
        files: Vec::new(),
        bytes: 0,
    };
    for _ in 0..MAX_ENTRIES {
        let Some(relative) = pending.pop_front() else {
            break;
        };
        ensure!(
            relative.components().count() <= 32,
            "backup depth exceeds32"
        );
        for child in fs::read_dir(root.join(&relative))? {
            let child = child?;
            let path = relative.join(child.file_name());
            ensure!(
                result.directories.len().saturating_add(result.files.len()) < MAX_ENTRIES,
                "backup exceeds20000entries"
            );
            let metadata = fs::symlink_metadata(child.path())?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "backup cannot contain links"
            );
            if metadata.is_dir() {
                result.directories.push(path.clone());
                pending.push_back(path);
            } else {
                ensure!(metadata.is_file(), "backup cannot contain special files");
                result.bytes = result
                    .bytes
                    .checked_add(metadata.len())
                    .context("backup byte overflow")?;
                ensure!(result.bytes <= MAX_BYTES, "backup exceeds4GiB");
                let (bytes, sha256) = io::file_digest(&child.path(), MAX_BYTES)?;
                result.files.push(Entry {
                    path,
                    bytes,
                    sha256,
                });
            }
        }
    }
    ensure!(pending.is_empty(), "backup traversal exceeded bound");
    result.directories.sort();
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

fn copy(root: &Path, destination: &Path, inventory: &Inventory) -> Result<()> {
    ensure!(
        !destination.try_exists()?,
        "restored backup root must be new"
    );
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    inventory
        .directories
        .iter()
        .try_for_each(|directory| -> Result<()> {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(destination.join(directory))?;
            Ok(())
        })?;
    inventory.files.iter().try_for_each(|entry| -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut source = File::open(root.join(&entry.path))?
            .take(entry.bytes.checked_add(1).context("backup read overflow")?);
        let mut target = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination.join(&entry.path))?;
        ensure!(
            std::io::copy(&mut source, &mut target)? == entry.bytes,
            "stopped backup source changed"
        );
        target.sync_all()?;
        Ok(())
    })?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

pub(super) fn restore(evidence: &Path, config: &Path, id: &str) -> Result<Value> {
    let original = evidence.join("server-state");
    let restored = evidence.join("restored-server-state");
    let before = inventory(&original)?;
    ensure!(!before.files.is_empty(), "stopped server backup is empty");
    let space = rustix::fs::statvfs(evidence)?;
    let available = space
        .f_bavail
        .checked_mul(space.f_frsize)
        .context("backup free-space overflow")?;
    ensure!(
        available
            >= before
                .bytes
                .checked_add(512 * 1024 * 1024)
                .context("backup reserve overflow")?,
        "insufficient disk headroom for backup clone"
    );
    copy(&original, &restored, &before)?;
    let cloned = inventory(&restored)?;
    ensure!(
        cloned == before,
        "restored backup differs from stopped original"
    );
    let hash = io::digest(&serde_json::to_vec(&before)?);
    let manifest = json!({"schema_revision":1,"pending_invocation_id":id,"source_root":original,"restored_root":restored,"file_count":before.files.len(),"directory_count":before.directories.len(),"total_bytes":before.bytes,"inventory":before,"inventory_sha256":hash,"restored_inventory_sha256":hash,"restored_file_count":cloned.files.len(),"restored_total_bytes":cloned.bytes,"both_processes_stopped_during_copy":true,"original_server_state_preserved":true,"scope":"single-node stopped-server backup restore; PostgreSQL and worker artifacts retained"});
    let path = evidence.join("server-backup-manifest.json");
    io::save(&path, &manifest)?;
    let old = String::from_utf8(io::read(config, io::JSON_LIMIT)?)?;
    let expected = format!("base-dir = {}", serde_json::to_string(&original)?);
    let replacement = format!("base-dir = {}", serde_json::to_string(&restored)?);
    ensure!(
        old.lines().filter(|line| *line == expected).count() == 1,
        "backup config must bind exactly one original directory"
    );
    let changed = old
        .lines()
        .map(|line| {
            if line == expected {
                replacement.as_str()
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let temporary = config.with_extension("restored.tmp");
    io::write(&temporary, changed.as_bytes())?;
    fs::rename(&temporary, config)?;
    File::open(evidence)?.sync_all()?;
    Ok(
        json!({"pending_invocation_id":id,"manifest":path,"manifest_sha256":io::file_digest(&path,io::JSON_LIMIT)?.1,"inventory_sha256":hash,"restored_inventory_sha256":hash,"file_count":cloned.files.len(),"total_bytes":cloned.bytes,"source_root":original,"restored_root":restored,"original_config_sha256":io::digest(old.as_bytes()),"restored_config_sha256":io::digest(changed.as_bytes()),"only_config_change":"base-dir"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_backup_preserves_original_and_rejects_links() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("server-state");
        fs::create_dir(&source)?;
        io::write(&source.join("ledger"), b"retained pending invocation")?;
        let config = root.path().join("restate.toml");
        io::write(
            &config,
            format!("base-dir = {}\n", serde_json::to_string(&source)?).as_bytes(),
        )?;
        let proof = restore(root.path(), &config, "inv_owned")?;
        assert_eq!(
            fs::read(source.join("ledger"))?,
            fs::read(root.path().join("restored-server-state/ledger"))?
        );
        assert_eq!(
            proof.get("inventory_sha256"),
            proof.get("restored_inventory_sha256")
        );
        std::os::unix::fs::symlink("ledger", source.join("link"))?;
        assert!(inventory(&source).is_err());
        Ok(())
    }
}
