import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import xml.etree.ElementTree as ET


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def http(base, route, value=None, key=None, timeout=10):
    headers = {"Accept": "application/json"}
    if key:
        headers["idempotency-key"] = key
    data = None if value is None else json.dumps(value).encode()
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(base + route, data=data, headers=headers)
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(request, timeout=timeout) as response:
            body = response.read(4 * 1024 * 1024 + 1)
            status = response.status
    except urllib.error.HTTPError as error:
        status, body = error.code, error.read(4 * 1024 * 1024 + 1)
    if len(body) > 4 * 1024 * 1024:
        raise RuntimeError("local Restate response exceeds 4 MiB")
    try:
        return status, json.loads(body) if body else None
    except json.JSONDecodeError:
        return status, body.decode(errors="replace")


def wait_http(base, route, deadline=90):
    until = time.monotonic() + deadline
    while time.monotonic() < until:
        try:
            status, value = http(base, route)
            if status == 200:
                return value
        except (OSError, TimeoutError):
            pass
        time.sleep(0.25)
    raise RuntimeError("local Restate health route " + route + " did not become ready within deadline")


def wait_port(number, process, deadline=60):
    until = time.monotonic() + deadline
    while time.monotonic() < until:
        if process.poll() is not None:
            raise RuntimeError("owned process exited before becoming ready")
        try:
            with socket.create_connection(("127.0.0.1", number), timeout=0.2):
                return
        except OSError:
            time.sleep(0.2)
    raise RuntimeError("owned process did not bind loopback before deadline")


def start(arguments, cwd, environment, log):
    with log.open("ab") as handle:
        return subprocess.Popen(arguments, cwd=cwd, env=environment,
            stdout=handle, stderr=subprocess.STDOUT, start_new_session=True)


def stop(process, graceful=True):
    if process is None:
        return True
    if process.poll() is not None:
        return process.returncode == 0
    if graceful:
        os.killpg(process.pid, signal.SIGCONT)
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=30)
            return process.returncode == 0
        except subprocess.TimeoutExpired:
            pass
    os.killpg(process.pid, signal.SIGKILL)
    process.wait(timeout=10)
    return False


def database_url(original, schema):
    url = urllib.parse.urlsplit(original)
    if url.scheme not in ("postgres", "postgresql"):
        raise RuntimeError("TEST_DATABASE_URL must be PostgreSQL")
    query = [(key, value) for key, value in urllib.parse.parse_qsl(url.query)
        if key != "options"]
    query.append(("options", "-csearch_path=" + schema))
    return urllib.parse.urlunsplit(url._replace(query=urllib.parse.urlencode(query)))


def postgres_environment(database):
    url = urllib.parse.urlsplit(database)
    if url.scheme not in ("postgres", "postgresql"):
        raise RuntimeError("TEST_DATABASE_URL must be PostgreSQL")
    environment = {key: value for key, value in os.environ.items()
        if not key.startswith("PG") and key not in ("DATABASE_URL", "TEST_DATABASE_URL")}
    values = {
        "PGHOST": urllib.parse.unquote(url.hostname) if url.hostname else None,
        "PGPORT": str(url.port) if url.port else None,
        "PGDATABASE": urllib.parse.unquote(url.path[1:]) if url.path else None,
        "PGUSER": urllib.parse.unquote(url.username) if url.username else None,
        "PGPASSWORD": urllib.parse.unquote(url.password) if url.password is not None else None,
        "PGCONNECT_TIMEOUT": "10",
    }
    fields = {"host": "PGHOST", "port": "PGPORT", "dbname": "PGDATABASE",
        "user": "PGUSER", "password": "PGPASSWORD", "options": "PGOPTIONS",
        "sslmode": "PGSSLMODE", "ssl-mode": "PGSSLMODE", "sslcert": "PGSSLCERT",
        "sslkey": "PGSSLKEY", "sslrootcert": "PGSSLROOTCERT", "sslcrl": "PGSSLCRL",
        "connect_timeout": "PGCONNECT_TIMEOUT", "application_name": "PGAPPNAME",
        "target_session_attrs": "PGTARGETSESSIONATTRS", "channel_binding": "PGCHANNELBINDING"}
    for key, value in urllib.parse.parse_qsl(url.query, keep_blank_values=True):
        if key in fields:
            values[fields[key]] = value
    environment.update({key: value for key, value in values.items() if value is not None})
    return environment


def sql(database, statement):
    binary = shutil.which("psql")
    if binary is None:
        candidate = Path("/opt/homebrew/opt/postgresql@18/bin/psql")
        binary = str(candidate) if candidate.is_file() else None
    if binary is None:
        raise RuntimeError("psql executable is required for isolated PostgreSQL proof")
    environment = postgres_environment(database)
    result = subprocess.run([binary, "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1"],
        input=statement, text=True, env=environment, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, timeout=30, check=False)
    if result.returncode:
        detail = result.stderr.strip().replace(database, "[REDACTED]")
        sensitive = [value for key, value in environment.items()
            if key in ("PGUSER", "PGPASSWORD", "PGDATABASE") and value]
        for value in sorted(sensitive, key=len, reverse=True):
            detail = detail.replace(value, "[REDACTED]")
        raise RuntimeError("isolated PostgreSQL statement failed (exit " + str(result.returncode)
            + "): " + detail[:2048])
    return json.loads(result.stdout) if result.stdout.strip() else None


def invocation(value):
    if not isinstance(value, dict):
        raise RuntimeError("Restate send returned no invocation descriptor")
    identity = value.get("invocationId", value.get("invocation_id", value.get("id")))
    if not isinstance(identity, str) or not identity:
        raise RuntimeError("Restate send returned no invocation ID")
    return identity


def submit(base, request):
    status, result = http(base, "/restate/send/AthleticOperations/execute",
        request, request["operation_id"])
    if status not in (200, 202):
        raise RuntimeError("Restate operation submission failed with HTTP " + str(status))
    return invocation(result), result


def output(base, identity, deadline=180, field="receipt"):
    until = time.monotonic() + deadline
    while time.monotonic() < until:
        try:
            status, value = http(base, "/restate/output/" + identity, timeout=5)
            if status == 200 and isinstance(value, dict) and field in value:
                return value
            if status not in (200, 202, 404, 409, 470, 503):
                raise RuntimeError("Restate invocation output failed with HTTP " + str(status))
        except (OSError, TimeoutError):
            pass
        time.sleep(0.25)
    raise RuntimeError("Restate invocation did not complete within bounded deadline")


def artifact(descriptor, root):
    path = Path(descriptor["path"]).resolve()
    if not path.is_relative_to(root.resolve()):
        raise RuntimeError("operation returned artifact outside owned root")
    data = path.read_bytes()
    if len(data) != descriptor["bytes"] or sha256(data) != descriptor["sha256"]:
        raise RuntimeError("operation artifact hash or byte count mismatch")
    return path, data


def workbook(path, review_rows):
    from recovery_review import verify_review

    namespace = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
    with zipfile.ZipFile(path) as archive:
        book = ET.fromstring(archive.read("xl/workbook.xml"))
        names = [sheet.attrib["name"] for sheet in book.findall(namespace + "sheets/" + namespace + "sheet")]
        if names != ["Athletes", "Review"]:
            raise RuntimeError("export must contain exactly Athletes then Review sheets")
        strings = []
        if "xl/sharedStrings.xml" in archive.namelist():
            shared = ET.fromstring(archive.read("xl/sharedStrings.xml"))
            strings = ["".join(text.text or "" for text in item.iter(namespace + "t"))
                for item in shared.findall(namespace + "si")]
        sheet = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
        rows = sheet.findall(namespace + "sheetData/" + namespace + "row")
        if len(rows) != 2:
            raise RuntimeError("export must contain exactly one confirmed-cohort athlete row")
        decoded = [decode_row(row, namespace, strings) for row in rows]
        cells = {title: decoded[1].get(column, "") for column, title in decoded[0].items()}
        if cells.get("Name") != "Brice Fuller" or cells.get("Graduation year") != "2028":
            raise RuntimeError("export did not preserve explicit Brice Fuller Class 2028 identity")
        if any("Kingston Penn" in value for value in cells.values()):
            raise RuntimeError("export included an athlete without a published cohort year")
        expected = {"Outdoor 1600m PR [all levels; milesplit]": "4:12.69",
            "Outdoor 1600m PR [ms; milesplit]": "4:33.74",
            "Indoor 3000m PR [hs; milesplit]": "8:26.83",
            "XC 5K PR [hs; milesplit]": "14:49.06"}
        for column, mark in expected.items():
            if not cells.get(column, "").startswith(mark):
                raise RuntimeError("export lost the published PR in " + column)
        review = ET.fromstring(archive.read("xl/worksheets/sheet2.xml"))
        review = verify_review(review.findall(namespace + "sheetData/" + namespace + "row"),
            namespace, strings, decode_row, review_rows)
        return {"sheets": names, "rows_including_header": len(rows), "review": review,
            "athlete": "Brice Fuller", "published_graduation_year": 2028,
            "unknown_cohort_excluded": "Kingston Penn", "verified_pr_cells": expected}


def decode_row(row, namespace, strings):
    result = {}
    for cell in row.findall(namespace + "c"):
        reference = cell.attrib["r"]
        column = "".join(character for character in reference if character.isalpha())
        value = cell.find(namespace + "v")
        if cell.attrib.get("t") == "s":
            text = strings[int(value.text)]
        elif cell.attrib.get("t") == "inlineStr":
            text = "".join(item.text or "" for item in cell.iter(namespace + "t"))
        else:
            text = value.text if value is not None and value.text is not None else ""
        result[column] = text
    return result
