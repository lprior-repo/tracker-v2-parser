#!/usr/bin/env python3
import argparse
import datetime
import gzip
import hashlib
import json
import os
import pathlib
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import unicodedata
from urllib.parse import quote, urlencode

from common import FORMAT, MAX_CONFIG_BYTES, TABLES, MigrationError, credential_filename, inventory, pg_env, pg_run, server_version, validate_database

MAX_MEMBERS = 250_000
MAX_METADATA = 128 * 1024 * 1024
MAX_MANIFEST = 64 * 1024 * 1024
MAX_SMALL_FILE = MAX_CONFIG_BYTES
MAX_PAYLOAD = 2 * 1024**4
HEX = re.compile(r"[0-9a-f]{64}\Z")
FIXED = {"database.dump", "config.toml", "inventory.json", "scrape.toml"}


class BoundedTarInfo(tarfile.TarInfo):
    def _proc_member(self, archive):
        archive.migration_headers = getattr(archive, "migration_headers", 0) + 1
        archive.migration_metadata = getattr(archive, "migration_metadata", 0) + 512 + len(self.name.encode("utf-8"))
        if archive.migration_headers > MAX_MEMBERS * 2 or archive.migration_metadata > MAX_METADATA:
            raise MigrationError("Archive metadata exceeds the supported limit")
        if self.type == tarfile.XHDTYPE:
            if getattr(archive, "migration_pax", False) or not 0 <= self.size <= 65536:
                raise MigrationError("Archive extended metadata is nested or exceeds the supported limit")
            archive.migration_metadata += self.size
            if archive.migration_metadata > MAX_METADATA:
                raise MigrationError("Archive metadata exceeds the supported limit")
        elif self.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
            raise MigrationError("Archive contains a link, device, sparse file, or unsupported header")
        result = super()._proc_member(archive)
        if result is not None and (result.sparse is not None or set(result.pax_headers) - {"path", "mtime", "atime", "ctime"}):
            raise MigrationError("Archive contains unsupported extended metadata")
        return result

    def _proc_pax(self, archive):
        raw = archive.fileobj.read(self._block(self.size))
        if len(raw) != self._block(self.size):
            raise MigrationError("Archive extended metadata is truncated")
        data = raw[:self.size]
        headers = {}
        position = 0
        while position < len(data):
            space = data.find(b" ", position, position + 12)
            length_text = data[position:space] if space >= 0 else b""
            if not length_text or not length_text.isdigit() or len(length_text) > 10:
                raise MigrationError("Archive extended metadata has invalid record lengths")
            end = position + int(length_text)
            if end <= space + 2 or end > len(data) or data[end - 1:end] != b"\n":
                raise MigrationError("Archive extended metadata has an invalid record")
            key_bytes, equals, value_bytes = data[space + 1:end - 1].partition(b"=")
            try:
                key = key_bytes.decode("ascii")
                value = value_bytes.decode("utf-8")
            except UnicodeError as error:
                raise MigrationError("Archive extended metadata has invalid text") from error
            if not equals or key not in {"path", "mtime", "atime", "ctime"} or key in headers:
                raise MigrationError("Archive extended metadata contains unsupported or duplicate keys")
            headers[key] = value
            position = end
        archive.migration_pax = True
        try:
            following = self.fromtarfile(archive)
        finally:
            archive.migration_pax = False
        if "path" in headers:
            following.name = headers["path"].rstrip("/") if following.isdir() else headers["path"]
        following.pax_headers = headers
        following.offset = self.offset
        return following


def relative_path(value):
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > 4096:
        raise MigrationError("Archive contains an invalid or oversized path")
    if "\\" in value or any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise MigrationError("Archive contains a nonportable path")
    pieces = value.split("/")
    if any(not part or part in (".", "..") or part.startswith(".") or ":" in part for part in pieces):
        raise MigrationError("Archive contains an absolute, hidden, or traversal path")
    for piece in pieces:
        if credential_filename(piece):
            raise MigrationError("Archive contains a credential-like filename")
    return value


def payload_path(value, directory=False):
    relative_path(value)
    pieces = value.split("/")
    if directory:
        if value not in ("evidence", "exports") and not (len(pieces) > 1 and pieces[0] == "exports"):
            raise MigrationError("Archive contains an unlisted directory")
    elif value == "manifest.json" or value in FIXED:
        return value
    elif len(pieces) == 2 and pieces[0] == "evidence" and re.fullmatch(r"[0-9a-f]{64}\.body", pieces[1]):
        return value
    elif len(pieces) > 1 and pieces[0] == "exports":
        return value
    else:
        raise MigrationError("Archive contains an unsupported payload path")
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise MigrationError("JSON contains a duplicate key")
        result[key] = value
    return result


def load_json(path, limit):
    if path.stat().st_size > limit:
        raise MigrationError("JSON metadata exceeds the supported limit")
    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_object,
                          parse_constant=lambda value: (_ for _ in ()).throw(MigrationError("Nonfinite JSON metadata")))
    except (ValueError, UnicodeError, RecursionError) as error:
        raise MigrationError("Invalid JSON metadata") from error


def validate_manifest(value, actual):
    keys = {"format", "created_at_utc", "source_postgres_major", "source_postgres_version", "database_dump", "config", "evidence_dir", "exports_dir", "inventory", "files"}
    if not isinstance(value, dict) or set(value) != keys or value["format"] != FORMAT:
        raise MigrationError("Invalid migration manifest format")
    for field, expected in (("database_dump", "database.dump"), ("config", "config.toml"), ("evidence_dir", "evidence"), ("exports_dir", "exports"), ("inventory", "inventory.json")):
        if value[field] != expected:
            raise MigrationError("Manifest uses an unsupported fixed path")
    major = value["source_postgres_major"]
    version = value["source_postgres_version"]
    if type(major) is not int or not 10 <= major <= 99 or not isinstance(version, str) or len(version) > 256 or not re.fullmatch(r"[0-9][^\x00-\x1f\x7f]*", version):
        raise MigrationError("Manifest has invalid PostgreSQL version metadata")
    if not version.startswith(str(major) + ".") and version != str(major):
        raise MigrationError("Manifest PostgreSQL major and version disagree")
    try:
        timestamp = datetime.datetime.fromisoformat(value["created_at_utc"].replace("Z", "+00:00"))
        if timestamp.utcoffset() != datetime.timedelta(0):
            raise ValueError("not UTC")
    except (ValueError, TypeError, AttributeError, OverflowError) as error:
        raise MigrationError("Manifest creation time must be an ISO8601 UTC timestamp") from error
    entries = value["files"]
    if not isinstance(entries, list) or len(entries) > MAX_MEMBERS:
        raise MigrationError("Invalid manifest file list")
    declared = {}
    previous = ""
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"path", "bytes", "sha256"}:
            raise MigrationError("Invalid manifest file entry")
        path = payload_path(entry["path"])
        size, checksum = entry["bytes"], entry["sha256"]
        if path == "manifest.json" or path <= previous or type(size) is not int or not 0 <= size <= MAX_PAYLOAD or not isinstance(checksum, str) or not HEX.fullmatch(checksum):
            raise MigrationError("Manifest paths, sizes, checksums, or ordering are invalid")
        previous = path
        declared[path] = {"bytes": size, "sha256": checksum}
    if not {"database.dump", "config.toml", "inventory.json"} <= declared.keys() or declared != actual:
        raise MigrationError("Archive payload differs from manifest sizes, checksums, or file list")
    return value


def validate_inventory(value):
    if not isinstance(value, dict) or set(value) != {"tables"} or not isinstance(value["tables"], list) or len(value["tables"]) != len(TABLES):
        raise MigrationError("Invalid logical inventory")
    for table, expected in zip(value["tables"], TABLES):
        if not isinstance(table, dict) or set(table) != {"name", "rows", "sha256"} or table["name"] != expected or type(table["rows"]) is not int or table["rows"] < 0 or not isinstance(table["sha256"], str) or not HEX.fullmatch(table["sha256"]):
            raise MigrationError("Invalid logical table fingerprint")
    return value


def validate_config(path):
    if path.stat().st_size > MAX_SMALL_FILE:
        raise MigrationError("Application configuration exceeds the supported limit")
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeError as error:
        raise MigrationError("Configuration is not UTF-8") from error
    assignments = re.compile(r"(?im)^\s*[\"']?([a-z0-9_.-]+)[\"']?\s*=\s*(.*)$")
    for match in assignments.finditer(text):
        name = match[1].casefold()
        if any(word in name for word in ("password", "secret", "token", "credential", "database_url", "api_key", "private_key")):
            raise MigrationError("Application configuration contains a credential assignment")
    if re.search(r"(?i)[a-z][a-z0-9+.-]*://[^\s/\"']+@", text):
        raise MigrationError("Application configuration contains URL credentials")
    if re.search(r"(?i)-----BEGIN [A-Z ]*PRIVATE KEY-----|[?&](?:password|passwd|token|access_token|api_key|secret)=", text):
        raise MigrationError("Application configuration contains private key or secret query data")


def unpack(archive_path, root):
    actual, seen, portable = {}, set(), {}
    total = 0
    with archive_path.open("rb") as source, gzip.GzipFile(fileobj=source, mode="rb") as compressed:
        with tarfile.open(fileobj=compressed, mode="r|", tarinfo=BoundedTarInfo) as archive:
            for member in archive:
                path = payload_path(member.name, member.isdir())
                if path in seen or len(seen) >= MAX_MEMBERS:
                    raise MigrationError("Archive has duplicate or too many members")
                pieces = path.split("/")
                for length in range(1, len(pieces) + 1):
                    prefix = "/".join(pieces[:length])
                    folded = unicodedata.normalize("NFC", prefix).casefold()
                    if folded in portable and portable[folded] != prefix:
                        raise MigrationError("Archive has case-colliding or Unicode-colliding paths")
                    portable[folded] = prefix
                seen.add(path)
                destination = root.joinpath(*path.split("/"))
                for ancestor in pathlib.PurePosixPath(path).parents:
                    if str(ancestor) != "." and str(ancestor) in actual:
                        raise MigrationError("Archive uses a file as a parent directory")
                if member.isdir():
                    if member.size:
                        raise MigrationError("Archive directory has payload bytes")
                    destination.mkdir(mode=0o700, parents=True, exist_ok=True)
                    continue
                if not member.isreg() or member.size < 0:
                    raise MigrationError("Archive contains a non-regular payload")
                limit = MAX_MANIFEST if path == "manifest.json" else MAX_SMALL_FILE if path in ("config.toml", "scrape.toml", "inventory.json") else MAX_PAYLOAD
                total += member.size
                if member.size > limit or total > MAX_PAYLOAD:
                    raise MigrationError("Archive payload exceeds the supported limit")
                destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                digest = hashlib.sha256()
                stream = archive.extractfile(member)
                if stream is None:
                    raise MigrationError("Archive payload cannot be read")
                remaining = member.size
                with stream, destination.open("xb") as output:
                    os.fchmod(output.fileno(), 0o600)
                    while remaining:
                        block = stream.read(min(1024 * 1024, remaining))
                        if not block:
                            raise MigrationError("Archive payload is truncated")
                        output.write(block)
                        digest.update(block)
                        remaining -= len(block)
                checksum = digest.hexdigest()
                if path.startswith("evidence/") and pathlib.PurePosixPath(path).name != checksum + ".body":
                    raise MigrationError("Evidence filename does not match its content SHA-256")
                if path != "manifest.json":
                    actual[path] = {"bytes": member.size, "sha256": checksum}
            trailer_size = 0
            while True:
                block = archive.fileobj.read(65536)
                if not block:
                    break
                trailer_size += len(block)
                if trailer_size > MAX_SMALL_FILE or any(block):
                    raise MigrationError("Archive has unexpected trailing data")
        while True:
            block = compressed.read(65536)
            if not block:
                break
            trailer_size += len(block)
            if trailer_size > MAX_SMALL_FILE or any(block):
                raise MigrationError("Archive has unexpected trailing data")
    if "manifest.json" not in seen or not (root / "evidence").is_dir() or not (root / "exports").is_dir():
        raise MigrationError("Archive is missing its manifest, evidence directory, or exports directory")
    manifest = validate_manifest(load_json(root / "manifest.json", MAX_MANIFEST), actual)
    expected = validate_inventory(load_json(root / "inventory.json", MAX_SMALL_FILE))
    validate_config(root / "config.toml")
    if (root / "scrape.toml").exists():
        validate_config(root / "scrape.toml")
    with (root / "database.dump").open("rb") as dump:
        if dump.read(5) != b"PGDMP":
            raise MigrationError("Database payload is not a PostgreSQL custom-format dump")
    return manifest, expected


def run_brew(brew, args):
    try:
        result = subprocess.run([brew, *args], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=1800, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise MigrationError("Homebrew command failed or exceeded its time limit; inspect Homebrew locally") from error
    if result.returncode:
        raise MigrationError("Homebrew command failed; inspect Homebrew locally (no output shown to protect local credentials)")
    return result.stdout.strip()


def bootstrap(major):
    if platform.system() != "Darwin":
        raise MigrationError("--bootstrap-macos is supported only on macOS; install PostgreSQL locally on Linux")
    host = pg_env().get("PGHOST", "localhost")
    if host not in ("localhost", "127.0.0.1", "::1") and not host.startswith("/"):
        raise MigrationError("Homebrew bootstrap requires a local PGHOST")
    brew = shutil.which("brew")
    if brew is None:
        raise MigrationError("Install Homebrew using its official instructions first; automatic Homebrew installation is not supported")
    formula = "postgresql@" + str(major)
    installed = run_brew(brew, ["list", "--formula", "--versions"])
    if not any(line.split()[0] == formula for line in installed.splitlines() if line.split()):
        run_brew(brew, ["install", formula])
    prefix = run_brew(brew, ["--prefix", formula])
    if not prefix or "\n" in prefix or not pathlib.Path(prefix).is_absolute():
        raise MigrationError("Homebrew returned an invalid PostgreSQL prefix")
    os.environ["PATH"] = str(pathlib.Path(prefix) / "bin") + os.pathsep + os.environ.get("PATH", "")
    print("Homebrew bootstrap may install/start " + formula + "; these local resources are retained if restore fails.")
    run_brew(brew, ["services", "start", formula])
    deadline = time.monotonic() + 60
    environment = pg_env()
    environment["PGCONNECT_TIMEOUT"] = "2"
    readiness = shutil.which("pg_isready")
    if readiness is None:
        raise MigrationError("The Homebrew PostgreSQL installation is missing pg_isready")
    while time.monotonic() < deadline:
        try:
            ready = subprocess.run([readiness, "--dbname=postgres", "--timeout=2"], env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=4, check=False)
            if ready.returncode == 0:
                return
        except subprocess.TimeoutExpired:
            pass
        time.sleep(1)
    raise MigrationError("Local PostgreSQL did not become ready within 60 seconds; check its service and local role credentials")


def database_exists(database):
    result = pg_run("psql", ["--dbname", "postgres"], sql="SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = '" + database.replace("'", "''") + "');")
    if result.strip() not in ("t", "f"):
        raise MigrationError("Cannot determine whether the target database already exists")
    return result.strip() == "t"


def target_url(database, role):
    settings = pg_env()
    host = settings.get("PGHOST", "localhost")
    port = settings.get("PGPORT", "5432")
    if not host or "," in host or any(ord(char) < 32 for char in host) or not port.isdigit() or not 0 < int(port) < 65536:
        raise MigrationError("Use a single PGHOST and a valid numeric PGPORT for the target application")
    if settings.get("PGHOSTADDR"):
        raise MigrationError("Unset PGHOSTADDR and use PGHOST so the generated application URL addresses the same target")
    options = {}
    if host.startswith("/"):
        options["host"] = host
        authority = "localhost"
    else:
        if not re.fullmatch(r"[A-Za-z0-9.:-]+", host):
            raise MigrationError("PGHOST must be a hostname, IPv6 address, or absolute Unix socket directory")
        authority = "[" + host + "]" if ":" in host else host
    sslmode = settings.get("PGSSLMODE")
    if sslmode:
        if sslmode not in ("disable", "allow", "prefer", "require", "verify-ca", "verify-full"):
            raise MigrationError("Invalid target PGSSLMODE")
        options["sslmode"] = sslmode
    url = "postgresql://" + quote(role, safe="") + "@" + authority + ":" + port + "/" + quote(database, safe="")
    return url + ("?" + urlencode(options) if options else "")


def dotenv_value(value):
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$").replace("\n", "\\n") + '"'


def publish(root, destination, url):
    destination.mkdir(mode=0o700, parents=False, exist_ok=False)
    for name in ("evidence", "exports", "config.toml", "scrape.toml"):
        source = root / name
        if not source.exists():
            continue
        if source.is_dir():
            os.rename(source, destination / name)
        else:
            with source.open("rb") as incoming, (destination / name).open("xb") as outgoing:
                os.fchmod(outgoing.fileno(), 0o600)
                shutil.copyfileobj(incoming, outgoing, 1024 * 1024)
    with (destination / ".env").open("x", encoding="utf-8") as handle:
        os.fchmod(handle.fileno(), 0o600)
        handle.write("DATABASE_URL=" + dotenv_value(url) + "\n")
        handle.write("TRACK_SCRAPPER_EVIDENCE_DIR=" + dotenv_value(str(destination / "evidence")) + "\n")


def restore(args):
    database = validate_database(args.database)
    destination = pathlib.Path(os.path.abspath(os.path.expanduser(args.data_dir)))
    if os.path.lexists(destination):
        raise MigrationError("Target data-dir already exists; choose a new path")
    parent = destination.parent.resolve()
    staging_parent = parent
    missing_parents = []
    while not staging_parent.exists():
        missing_parents.append(staging_parent)
        staging_parent = staging_parent.parent
    if not staging_parent.is_dir():
        raise MigrationError("Target data-dir has a non-directory parent")
    destination = parent / destination.name
    archive_path = pathlib.Path(args.archive).expanduser()
    if archive_path.is_symlink() or not archive_path.is_file():
        raise MigrationError("Archive must be an existing regular file, not a symlink")
    if args.bootstrap_macos and platform.system() != "Darwin":
        raise MigrationError("--bootstrap-macos is supported only on macOS")
    created = False
    publishing = False
    created_parents = []
    with tempfile.TemporaryDirectory(prefix=".track-scrapper-restore-", dir=staging_parent) as staging:
        root = pathlib.Path(staging)
        os.chmod(root, 0o700)
        manifest, expected = unpack(archive_path, root)
        major = manifest["source_postgres_major"]
        if os.path.lexists(destination):
            raise MigrationError("Target data-dir appeared during validation; refusing to overwrite it")
        if args.bootstrap_macos:
            bootstrap(major)
        for tool in ("psql", "createdb", "pg_restore"):
            if shutil.which(tool) is None:
                raise MigrationError("Missing PostgreSQL tool: " + tool + "; install PostgreSQL tools or use --bootstrap-macos on macOS")
        if database_exists(database):
            raise MigrationError("Target database already exists; choose a new database name")
        target_major, _ = server_version("postgres")
        if target_major != major:
            raise MigrationError("Target PostgreSQL major must match source major " + str(major) + " for exact logical fingerprint verification")
        version = pg_run("pg_restore", ["--version"])
        match = re.search(r"\b(\d+)(?:\.\d+)", version)
        if match is None or int(match[1]) != major:
            raise MigrationError("pg_restore must match PostgreSQL major " + str(major))
        role_result = pg_run("psql", ["--dbname", "postgres"], sql="SELECT to_json(current_user)::text;")
        try:
            role = json.loads(role_result)
        except (ValueError, TypeError) as error:
            raise MigrationError("Cannot determine the target PostgreSQL role") from error
        if not isinstance(role, str) or not role or any(ord(c) < 32 for c in role):
            raise MigrationError("Target PostgreSQL role is invalid")
        url = target_url(database, role)
        try:
            pg_run("createdb", ["--maintenance-db=postgres", "--template=template0", "--encoding=UTF8", database])
            created = True
            if os.path.lexists(destination):
                raise MigrationError("Target data-dir appeared during database creation; refusing to overwrite it")
            pg_run("pg_restore", ["--dbname", database, "--no-owner", "--no-privileges", "--single-transaction", "--exit-on-error", "--clean", "--if-exists", str(root / "database.dump")])
            if inventory(database) != expected:
                raise MigrationError("Restored database logical fingerprints do not exactly match the source inventory")
            if os.path.lexists(destination):
                raise MigrationError("Target data-dir appeared during restore; refusing to overwrite it")
            publishing = True
            for directory in reversed(missing_parents):
                directory.mkdir(mode=0o700, exist_ok=False)
                created_parents.append(directory)
            publish(root, destination, url)
        except BaseException:
            if created:
                print("Restore failed. Newly created database " + database + " is retained as a partial target; it was not dropped or cleaned up.", file=sys.stderr)
            else:
                print("Database creation did not complete successfully; inspect target existence before retrying. No database was dropped.", file=sys.stderr)
            if publishing:
                print("Data-dir publication was attempted at " + shlex.quote(str(destination)) + "; any partial directory is retained, not a completed restore.", file=sys.stderr)
            for directory in created_parents:
                print("New parent directory retained: " + shlex.quote(str(directory)), file=sys.stderr)
            raise
    repository = pathlib.Path(__file__).resolve().parents[2]
    export = destination / "exports" / ("restored-" + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ") + ".xlsx")
    command = "cd " + shlex.quote(str(destination)) + " && " + shlex.join(["env", "-u", "DATABASE_URL", "-u", "TRACK_SCRAPPER_EVIDENCE_DIR", "cargo", "run", "--manifest-path", str(repository / "Cargo.toml"), "--", "export", "--config", str(destination / "config.toml"), "--output", str(export)])
    print("Restore complete: database " + database + "; all " + str(len(TABLES)) + " logical table fingerprints match.")
    print("Private application data: " + shlex.quote(str(destination)))
    print("Source metadata and original artifact paths were preserved; evidence is located by SHA-256 basename.")
    print("The new .env has no password. Use the target role's existing PGPASSFILE/.pgpass, or privately configure its local DATABASE_URL password; never copy source credentials.")
    print("Export without scraping (loads the new private .env):\n" + command)


def main():
    parser = argparse.ArgumentParser(description="Hydrate a trusted track-scrapper migration bundle into a NEW PostgreSQL database and data-dir. A dump executes SQL: only use bundles from your trusted source and a least-privilege target role; checksums are not authentication.")
    parser.add_argument("--archive", required=True, help="Trusted .tar.gz migration bundle")
    parser.add_argument("--database", required=True, help="New database name, never an existing database or URL")
    parser.add_argument("--data-dir", required=True, help="New private application directory; missing parents are created only after database verification")
    parser.add_argument("--bootstrap-macos", action="store_true", help="Use existing Homebrew to install/start the matching local PostgreSQL major (macOS only)")
    args = parser.parse_args()
    os.umask(0o077)
    try:
        restore(args)
    except (MigrationError, OSError, tarfile.TarError, EOFError, ValueError, RecursionError) as error:
        if isinstance(error, MigrationError):
            message = str(error)
        else:
            message = "Restore could not complete due to invalid archive data, filesystem access, or a missing directory; no existing database or data-dir was replaced"
        print("Error: " + message, file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("Restore interrupted; any reported partial target is retained", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
