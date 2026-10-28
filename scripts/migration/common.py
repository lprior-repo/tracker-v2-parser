import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import tempfile

FORMAT = "track-scrapper-migration-v1"
MAX_CONFIG_BYTES = 1024 * 1024
TABLES = (
    "_sqlx_migrations",
    "athletes",
    "athlete_sources",
    "schools",
    "results",
    "contacts",
    "scrape_pages",
)
SNAPSHOT = re.compile(r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{8}-[0-9]+")
SECRET_FILENAME = re.compile(
    r"(?:^|[._ -])(?:credentials?|secrets?|tokens?|passwords?|passwd|pgpass|"
    r"pg_service|id_rsa|id_ed25519|cookies?|browser|profiles?|login data)(?:$|[._ -])",
    re.IGNORECASE,
)


def credential_filename(name):
    return bool(SECRET_FILENAME.search(name)) or name.casefold().endswith(
        (".pem", ".key", ".p12", ".pfx", ".pgpass")
    )


class MigrationError(Exception):
    pass


def validate_database(name):
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]{0,62}", name):
        raise MigrationError("database must be a name of at most 63 ASCII characters, not a connection URL")
    return name


def pg_env():
    result = dict(os.environ)
    if result.get("PGSERVICE"):
        raise MigrationError("PGSERVICE is not supported; unset it and use explicit PGHOST, PGPORT, PGUSER and a private PGPASSFILE")
    result.setdefault("PGHOST", "localhost")
    result.setdefault("PGCONNECT_TIMEOUT", "15")
    result.setdefault("PGAPPNAME", "track-scrapper-migration")
    return result


def _detail(value):
    text = value.decode("utf-8", errors="replace") if isinstance(value, bytes) else str(value)
    for key in ("PGPASSWORD", "DATABASE_URL", "TEST_DATABASE_URL"):
        secret = os.environ.get(key)
        if secret:
            text = text.replace(secret, "[redacted]")
    return text.strip()[:2000]


def _command(tool):
    executable = shutil.which(tool)
    if executable is None:
        raise MigrationError(f"required PostgreSQL tool is missing from PATH: {tool}")
    result = [executable]
    if tool in {"psql", "pg_dump", "pg_restore", "createdb", "dropdb"}:
        result.append("--no-password")
    if tool == "psql":
        result.extend(["--no-psqlrc", "--quiet", "--tuples-only", "--no-align", "--set=ON_ERROR_STOP=1"])
    return result


def pg_run(tool, args, *, sql=None):
    prefix = _command(tool)
    command = prefix[:1] + list(args) if list(args) in (["--version"], ["--help"]) else prefix + list(args)
    try:
        result = subprocess.run(command, input=sql, text=True, capture_output=True, env=pg_env(), timeout=1800)
    except subprocess.TimeoutExpired as error:
        raise MigrationError(f"{tool} exceeded its 30-minute deadline") from error
    except OSError as error:
        raise MigrationError(f"cannot run {tool}: {_detail(error)}") from error
    if result.returncode:
        raise MigrationError(f"{tool} failed (exit {result.returncode}): {_detail(result.stderr)}")
    return result.stdout


def server_version(database):
    validate_database(database)
    value = pg_run("psql", ["--dbname", database], sql="SELECT current_setting('server_version_num') || '|' || current_setting('server_version');")
    try:
        number, display = value.strip().split("|", 1)
        major = int(number) // 10000
    except (ValueError, TypeError) as error:
        raise MigrationError("PostgreSQL returned an invalid version") from error
    if major < 13:
        raise MigrationError("this application requires PostgreSQL 13 or newer")
    return major, display


def _finish(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)


@contextlib.contextmanager
def exported_snapshot(database):
    validate_database(database)
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen(_command("psql") + ["--dbname", database], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors, env=pg_env())
        try:
            process.stdin.write(b"SET idle_in_transaction_session_timeout='30min'; BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SELECT pg_export_snapshot();\n")
            process.stdin.flush()
            ready, _, _ = select.select([process.stdout], [], [], 30)
            if not ready:
                raise MigrationError("timed out while opening the PostgreSQL backup snapshot")
            snapshot = process.stdout.readline(128).decode("ascii", errors="replace").strip()
            if not SNAPSHOT.fullmatch(snapshot):
                _finish(process)
                errors.seek(0)
                raise MigrationError(f"cannot export PostgreSQL snapshot: {_detail(errors.read())}")
            yield snapshot
            process.stdin.write(b"ROLLBACK;\n\\q\n")
            process.stdin.close()
            try:
                status = process.wait(timeout=15)
            except subprocess.TimeoutExpired as error:
                raise MigrationError("backup snapshot session did not close within 15 seconds") from error
            if status:
                errors.seek(0)
                raise MigrationError(f"backup snapshot session failed: {_detail(errors.read())}")
        except (BrokenPipeError, OSError) as error:
            raise MigrationError(f"backup snapshot session failed: {_detail(error)}") from error
        finally:
            _finish(process)
            for stream in (process.stdin, process.stdout):
                if stream is not None and not stream.closed:
                    stream.close()


def _table_inventory(database, table, snapshot):
    key = "version" if table == "_sqlx_migrations" else "id"
    attach = f"SET TRANSACTION SNAPSHOT '{snapshot}';" if snapshot is not None else ""
    sql = (
        "SET TimeZone='UTC'; SET DateStyle='ISO, YMD'; SET extra_float_digits=3; "
        "SET client_encoding='UTF8'; SET bytea_output='hex'; SET statement_timeout='10min'; "
        "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; "
        f"{attach} COPY (SELECT to_jsonb(t)::text FROM public.{table} t ORDER BY {key}) TO STDOUT; COMMIT;"
    )
    digest = hashlib.sha256()
    rows = 0
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen(_command("psql") + ["--dbname", database, "--command", sql], stdout=subprocess.PIPE, stderr=errors, env=pg_env())
        try:
            while True:
                block = process.stdout.read(1024 * 1024)
                if not block:
                    break
                digest.update(block)
                rows += block.count(b"\n")
            status = process.wait(timeout=15)
            if status:
                errors.seek(0)
                raise MigrationError(f"cannot fingerprint public.{table}: {_detail(errors.read())}")
        finally:
            _finish(process)
            process.stdout.close()
    return {"name": table, "rows": rows, "sha256": digest.hexdigest()}


def inventory(database, snapshot=None):
    validate_database(database)
    if snapshot is not None and not SNAPSHOT.fullmatch(snapshot):
        raise MigrationError("invalid exported snapshot identifier")
    return {"tables": [_table_inventory(database, table, snapshot) for table in TABLES]}


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_json(path, value):
    descriptor = os.open(Path(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
