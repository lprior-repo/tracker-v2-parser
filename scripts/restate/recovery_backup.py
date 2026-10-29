import hashlib
import json
import os
from pathlib import Path
import shutil
import stat

from recovery_support import save, sha256

MAX_ENTRIES = 20000
MAX_BYTES = 4 * 1024 * 1024 * 1024
MAX_DEPTH = 32
COPY_RESERVE_BYTES = 512 * 1024 * 1024


def file_digest(path, expected_bytes):
    digest = hashlib.sha256()
    consumed = 0
    with path.open("rb") as handle:
        while True:
            block = handle.read(1024 * 1024)
            if not block:
                break
            consumed += len(block)
            if consumed > expected_bytes or consumed > MAX_BYTES:
                raise RuntimeError("stopped server file changed or exceeds backup byte budget")
            digest.update(block)
    if consumed != expected_bytes:
        raise RuntimeError("stopped server file changed during backup inventory")
    return digest.hexdigest()


def walk_error(error):
    raise error


def inventory(root):
    if root.is_symlink() or not root.is_dir():
        raise RuntimeError("owned server backup root must be a real directory")
    files, directories, total = [], [], 0
    for current, children, names in os.walk(root, followlinks=False, onerror=walk_error):
        parent = Path(current)
        relative = parent.relative_to(root)
        if len(relative.parts) > MAX_DEPTH:
            raise RuntimeError("server backup exceeds directory depth budget")
        children.sort()
        names.sort()
        for name in children:
            child = parent / name
            if child.is_symlink() or not child.is_dir():
                raise RuntimeError("server backup cannot contain directory links or special files")
            directories.append(child.relative_to(root).as_posix())
        if len(files) + len(directories) + len(names) > MAX_ENTRIES:
            raise RuntimeError("server backup exceeds bounded entry count")
        for name in names:
            path = parent / name
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode):
                raise RuntimeError("server backup cannot contain links or special files")
            total += metadata.st_size
            if total > MAX_BYTES:
                raise RuntimeError("server backup exceeds 4 GiB total byte budget")
            files.append({"path": path.relative_to(root).as_posix(),
                "bytes": metadata.st_size, "sha256": file_digest(path, metadata.st_size)})
    return {"directories": sorted(directories),
        "files": sorted(files, key=lambda item: item["path"]), "bytes": total}


def restore_server_backup(evidence, config, invocation_id):
    original = evidence / "server-state"
    restored = evidence / "restored-server-state"
    if restored.exists() or restored.is_symlink():
        raise RuntimeError("owned restored server directory must be new")
    before = inventory(original)
    if not before["files"]:
        raise RuntimeError("stopped server backup is empty")
    if shutil.disk_usage(evidence).free < before["bytes"] + COPY_RESERVE_BYTES:
        raise RuntimeError("insufficient disk headroom for verified native server backup clone")
    shutil.copytree(original, restored, symlinks=False)
    cloned = inventory(restored)
    if cloned != before:
        raise RuntimeError("restored native server backup differs from stopped original inventory")
    manifest = {"schema_revision": 1, "pending_invocation_id": invocation_id,
        "source_root": str(original), "restored_root": str(restored),
        "file_count": len(before["files"]), "directory_count": len(before["directories"]),
        "total_bytes": before["bytes"], "inventory": before,
        "inventory_sha256": sha256(json.dumps(before, sort_keys=True, separators=(",", ":")).encode()),
        "restored_inventory_sha256": sha256(json.dumps(cloned, sort_keys=True, separators=(",", ":")).encode()),
        "restored_file_count": len(cloned["files"]), "restored_total_bytes": cloned["bytes"],
        "comparison": "all relative file paths, directories, byte counts and SHA-256 match",
        "both_processes_stopped_during_copy": True,
        "original_server_state_preserved": True,
        "scope": "single-node stopped-server backup restore; PostgreSQL schema and worker artifacts retained"}
    manifest_path = evidence / "server-backup-manifest.json"
    save(manifest_path, manifest)
    original_config = config.read_text()
    old = "base-dir = " + json.dumps(str(original))
    new = "base-dir = " + json.dumps(str(restored))
    lines = original_config.splitlines(keepends=True)
    if sum(line.rstrip("\n") == old for line in lines) != 1:
        raise RuntimeError("owned server config must bind exactly one original base directory")
    changed = "".join(new + "\n" if line.rstrip("\n") == old else line for line in lines)
    temporary = config.with_suffix(".restored.tmp")
    with temporary.open("x") as handle:
        handle.write(changed)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, config)
    return {"pending_invocation_id": invocation_id, "manifest": str(manifest_path),
        "manifest_sha256": sha256(manifest_path.read_bytes()),
        "inventory_sha256": manifest["inventory_sha256"],
        "restored_inventory_sha256": manifest["restored_inventory_sha256"],
        "file_count": len(before["files"]),
        "total_bytes": before["bytes"], "source_root": str(original),
        "restored_root": str(restored), "original_config_sha256": sha256(original_config.encode()),
        "restored_config_sha256": sha256(changed.encode()), "only_config_change": "base-dir"}
