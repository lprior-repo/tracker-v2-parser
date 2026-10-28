#!/usr/bin/env python3
import argparse
from datetime import datetime, timezone
import gzip
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
import tarfile
import tempfile
import unicodedata

from common import (
    FORMAT,
    MAX_CONFIG_BYTES,
    MigrationError,
    credential_filename,
    exported_snapshot,
    inventory,
    pg_run,
    server_version,
    sha256_file,
    validate_database,
    write_json,
)


SECRET_CONFIG = re.compile(
    r"[a-z][a-z0-9+.-]*://[^\s/\"']*@|"
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----|"
    r"[?&](?:password|token|secret|api[_-]?key)=",
    re.IGNORECASE,
)
SECRET_KEYS = ("password", "passwd", "secret", "token", "credential", "authorization", "cookie", "api_key", "api-key", "database_url", "database-url", "private_key", "private-key")
BACKUP_NAME = re.compile(r"(?:^|[._-])backups?(?:$|[._-])", re.IGNORECASE)
BACKUP_SUFFIXES = (".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".dump", ".sql", ".backup", ".bak")


def source_path(value, *, directory=False):
    raw = Path(value).expanduser().absolute()
    current = Path(raw.anchor)
    for part in raw.parts[1:]:
        if part == "..":
            raise MigrationError("Source paths must not contain '..'")
        current = current / part
        info = current.lstat()
        if stat.S_ISLNK(info.st_mode):
            raise MigrationError(f"Refusing symbolic-link source: {current}")
    info = raw.stat()
    expected = stat.S_ISDIR if directory else stat.S_ISREG
    if not expected(info.st_mode):
        kind = "directory" if directory else "regular file"
        raise MigrationError(f"Expected an existing {kind}: {raw}")
    return raw


def safe_name(name):
    if (
        not name
        or name.startswith(".")
        or "\\" in name
        or ":" in name
        or any(ord(character) < 32 or ord(character) == 127 for character in name)
        or credential_filename(name)
    ):
        raise MigrationError("Selected payload contains a hidden, unsafe, or credential-like name")


def is_within(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def signature(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def copy_payload(source, destination):
    before = source.lstat()
    if not stat.S_ISREG(before.st_mode):
        raise MigrationError(f"Source is no longer a regular file: {source}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    descriptor = os.open(source, flags)
    with os.fdopen(descriptor, "rb") as reader:
        opened = os.fstat(reader.fileno())
        if not stat.S_ISREG(opened.st_mode) or signature(before) != signature(opened):
            raise MigrationError(f"Source changed while opening: {source}")
        missing = []
        parent = destination.parent
        while not parent.exists():
            missing.append(parent)
            parent = parent.parent
        for parent in reversed(missing):
            parent.mkdir(mode=0o700)
        with destination.open("xb") as writer:
            os.fchmod(writer.fileno(), 0o600)
            shutil.copyfileobj(reader, writer, 1024 * 1024)
        if signature(opened) != signature(os.fstat(reader.fileno())):
            raise MigrationError(f"Source changed during backup: {source}")
    if signature(before) != signature(source.lstat()):
        raise MigrationError(f"Source changed during backup: {source}")


def check_config(path):
    if path.stat().st_size > MAX_CONFIG_BYTES:
        raise MigrationError("Configuration exceeds the 1 MiB safety limit")
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeError:
        raise MigrationError("Configuration must be UTF-8 text") from None
    for line in text.splitlines():
        key, assignment, _ = line.partition("=")
        if assignment and any(word in key.casefold() for word in SECRET_KEYS):
            raise MigrationError("Configuration contains credential-like settings; provide a non-secret application config")
    if SECRET_CONFIG.search(text):
        raise MigrationError("Configuration contains credential-like settings; provide a non-secret application config")


def export_parts(metadata, origin):
    parts = metadata.get("parts")
    if metadata.get("complete") is not True or not isinstance(parts, list) or not parts:
        raise MigrationError(f"Incomplete workbook export: {origin}")
    seen = set()
    for part in parts:
        if not isinstance(part, dict) or not isinstance(part.get("file"), str):
            raise MigrationError(f"Invalid workbook part in export manifest: {origin}")
        name = part["file"]
        safe_name(name)
        if "/" in name or name in seen:
            raise MigrationError(f"Invalid or duplicate workbook part in export manifest: {origin}")
        size, checksum = part.get("bytes"), part.get("sha256")
        if (
            type(size) is not int or size < 0
            or not isinstance(checksum, str)
            or not re.fullmatch(r"[0-9a-f]{64}", checksum)
        ):
            raise MigrationError(f"Invalid workbook part metadata: {origin}")
        seen.add(name)
    return parts


def export_sources(root, imported):
    selected = {}
    portable = {}

    def add(source, relative):
        for component in relative.parts:
            safe_name(component)
        for length in range(1, len(relative.parts) + 1):
            prefix = "/".join(relative.parts[:length])
            folded = unicodedata.normalize("NFC", prefix).casefold()
            if folded in portable and portable[folded] != prefix:
                raise MigrationError(f"Export paths collide on macOS: {portable[folded]} and {prefix}")
            portable[folded] = prefix
        if source.name.lower().endswith(BACKUP_SUFFIXES):
            raise MigrationError(f"Refusing an old backup in exports: {source}")
        if relative in selected:
            raise MigrationError(f"Duplicate export destination: exports/{relative.as_posix()}")
        selected[relative] = source_path(source)

    if root.exists() or root.is_symlink():
        source_path(root, directory=True)
        for directory, names, files in os.walk(root, followlinks=False, onerror=raise_walk_error):
            names.sort()
            files.sort()
            for name in names:
                safe_name(name)
                child = Path(directory) / name
                if BACKUP_NAME.search(name):
                    raise MigrationError(f"Refusing an old backup directory in exports: {child}")
                source_path(child, directory=True)
            for name in files:
                path = Path(directory) / name
                add(path, path.relative_to(root))
    for value in imported:
        source = source_path(value)
        add(source, Path("imported") / source.name)
    available = set(selected.values())
    for source in available:
        if source.suffix.lower() != ".xlsx":
            continue
        stem = re.sub(r"-[0-9]{4,}$", "", source.stem)
        exact = source.with_suffix(".manifest.json")
        for completion in {exact, source.with_name(stem + ".manifest.json")}:
            if not completion.exists() and not completion.is_symlink():
                continue
            source_path(completion)
            if completion.stat().st_size > 16 * 1024 * 1024:
                raise MigrationError(f"Export completion manifest exceeds 16 MiB: {completion}")
            try:
                metadata = json.loads(completion.read_text(encoding="utf-8"))
            except (ValueError, UnicodeError) as error:
                raise MigrationError(f"Invalid export completion manifest: {completion}") from error
            if not isinstance(metadata, dict) or metadata.get("format") != "track-scrapper-export-v1":
                continue
            parts = export_parts(metadata, completion)
            if completion != exact and not any(part["file"] == source.name for part in parts):
                continue
            if completion not in available:
                raise MigrationError(f"Select the completion manifest and every workbook part together: {completion}")
    return selected


def raise_walk_error(error):
    raise error


def check_export_sets(staging, files):
    for relative in files:
        if not relative.name.endswith(".manifest.json"):
            continue
        path = staging / "exports" / relative
        if path.stat().st_size > 16 * 1024 * 1024:
            raise MigrationError(f"Export manifest exceeds 16 MiB: {relative}")
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except (ValueError, UnicodeError):
            raise MigrationError(f"Invalid export completion manifest: {relative}") from None
        if not isinstance(value, dict) or value.get("format") != "track-scrapper-export-v1":
            continue
        for part in export_parts(value, relative):
            name = part["file"]
            destination = relative.parent / name
            if destination not in files:
                raise MigrationError(f"Select every part listed by export manifest {relative}; missing {name}")
            copied = staging / "exports" / destination
            if copied.stat().st_size != part["bytes"] or sha256_file(copied) != part["sha256"]:
                raise MigrationError(f"Workbook does not match its completion manifest: {destination}")


def copy_evidence(root, staging):
    source_path(root, directory=True)
    destination = staging / "evidence"
    destination.mkdir(mode=0o700)
    for source in sorted(root.iterdir()):
        info = source.lstat()
        if not stat.S_ISREG(info.st_mode):
            raise MigrationError(f"Evidence entry is not a regular file: {source}")
        if source.name.endswith(".tmp"):
            continue
        safe_name(source.name)
        if not re.fullmatch(r"[0-9a-f]{64}\.body", source.name):
            raise MigrationError(f"Evidence entry is not a SHA-256 .body file: {source}")
        target = destination / source.name
        copy_payload(source, target)
        if sha256_file(target) != source.stem:
            raise MigrationError(f"Evidence content does not match its SHA-256 basename: {source.name}")


def write_archive(staging, archive_path):
    with archive_path.open("xb") as raw:
        os.fchmod(raw.fileno(), 0o600)
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as archive:
                for path in sorted(staging.rglob("*"), key=lambda item: item.relative_to(staging).as_posix()):
                    relative = path.relative_to(staging).as_posix()
                    info = tarfile.TarInfo(relative)
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = 0
                    if path.is_dir():
                        info.type = tarfile.DIRTYPE
                        info.mode = 0o700
                        archive.addfile(info)
                    elif path.is_file() and not path.is_symlink():
                        info.size = path.stat().st_size
                        info.mode = 0o600
                        with path.open("rb") as reader:
                            archive.addfile(info, reader)
                    else:
                        raise MigrationError(f"Staged archive entry is not a regular file or directory: {relative}")
        raw.flush()
        os.fsync(raw.fileno())


def backup(args):
    database = validate_database(args.database)
    data = source_path(args.data_dir, directory=True)
    config = source_path(args.config)
    safe_name(config.name)
    source_path(data / "evidence", directory=True)
    output_raw = Path(args.output).expanduser().absolute()
    if ".." in output_raw.parts:
        raise MigrationError("Output path must not contain '..'")
    parent = source_path(output_raw.parent, directory=True)
    output = parent / output_raw.name
    if not output.name.endswith(".tar.gz"):
        raise MigrationError("Output must have a .tar.gz filename")
    if output.exists() or output.is_symlink():
        raise MigrationError(f"Output already exists; refusing overwrite: {output}")
    if is_within(output, data):
        raise MigrationError("Output must be outside the source application data directory")
    if output == config:
        raise MigrationError("Output must not replace the source configuration")
    exports = export_sources(data / "exports", args.export_file)
    if output in exports.values():
        raise MigrationError("Output must not be a selected export file")
    scrape = data / "scrape.toml"
    has_scrape = scrape.exists() or scrape.is_symlink()
    if has_scrape:
        source_path(scrape)
    major, version = server_version(database)
    dump_version = pg_run("pg_dump", ["--version"])
    match = re.search(r"\b(\d+)(?:\.\d+)*", dump_version)
    if match is None:
        raise MigrationError("Could not determine pg_dump major version")
    dump_major = int(match.group(1))
    if dump_major != major:
        raise MigrationError(f"pg_dump major {dump_major} does not match PostgreSQL server major {major}; install PostgreSQL {major} client tools and put their bin directory first on PATH so the archive is compatible with the same-major Mac restore client")
    with tempfile.TemporaryDirectory(prefix=".track-scrapper-migration-", dir=parent) as temporary:
        private = Path(temporary)
        staging = private / "payload"
        staging.mkdir(mode=0o700)
        copy_payload(config, staging / "config.toml")
        check_config(staging / "config.toml")
        if has_scrape:
            copy_payload(scrape, staging / "scrape.toml")
            check_config(staging / "scrape.toml")
        with exported_snapshot(database) as snapshot:
            pg_run("pg_dump", [
                "--dbname", database,
                "--format=custom",
                "--schema=public",
                "--no-owner",
                "--no-privileges",
                "--snapshot", snapshot,
                "--file", str(staging / "database.dump"),
            ])
            dump = staging / "database.dump"
            if not dump.is_file() or dump.stat().st_size == 0:
                raise MigrationError("pg_dump did not produce a nonempty custom-format dump")
            os.chmod(dump, 0o600)
            write_json(staging / "inventory.json", inventory(database, snapshot))
        copy_evidence(data / "evidence", staging)
        (staging / "exports").mkdir(mode=0o700)
        for relative, source in sorted(exports.items()):
            copy_payload(source, staging / "exports" / relative)
        check_export_sets(staging, exports)
        files = []
        for path in sorted(staging.rglob("*"), key=lambda item: item.relative_to(staging).as_posix()):
            if path.is_file():
                files.append({
                    "path": path.relative_to(staging).as_posix(),
                    "bytes": path.stat().st_size,
                    "sha256": sha256_file(path),
                })
        write_json(staging / "manifest.json", {
            "format": FORMAT,
            "created_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
            "source_postgres_major": major,
            "source_postgres_version": version,
            "database_dump": "database.dump",
            "config": "config.toml",
            "evidence_dir": "evidence",
            "exports_dir": "exports",
            "inventory": "inventory.json",
            "files": files,
        })
        archive_path = private / "bundle.partial"
        write_archive(staging, archive_path)
        archive_bytes = archive_path.stat().st_size
        try:
            os.link(archive_path, output)
        except FileExistsError:
            raise MigrationError(f"Output appeared during backup; refusing overwrite: {output}") from None
    print(f"Backup: {output}")
    print(f"PostgreSQL: {version} (major {major}); pg_dump major {dump_major}")
    print(f"Payload files: {len(files)}; export files: {len(exports)}; archive bytes: {archive_bytes}")


def main():
    parser = argparse.ArgumentParser(
        description="Back up PostgreSQL, retained evidence, configuration and selected exports into a private portable archive.",
        epilog="Supply PostgreSQL connection settings only through libpq environment variables (PGHOST, PGPORT, PGUSER, PGPASSWORD or PGPASSFILE). No source data is modified. Checksums provide integrity, not authenticity: transfer this private, unencrypted archive through a trusted channel and restore only trusted bundles using a least-privilege target role, never a PostgreSQL superuser.",
    )
    parser.add_argument("--database", required=True, help="Source PostgreSQL database name, not a URL")
    parser.add_argument("--data-dir", required=True, help="Existing application directory containing evidence and optional exports")
    parser.add_argument("--config", required=True, help="Non-secret application config.toml")
    parser.add_argument("--output", required=True, help="New .tar.gz archive outside the source data directory")
    parser.add_argument("--export-file", action="append", default=[], metavar="PATH", help="Additional export file; repeat for every workbook part and completion manifest")
    args = parser.parse_args()
    try:
        backup(args)
    except (MigrationError, OSError, tarfile.TarError) as error:
        print(f"Backup failed: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("Backup interrupted; no incomplete archive was published", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
