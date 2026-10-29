import argparse
import json
import os
from pathlib import Path
import signal
import sys
import time
import uuid

from recovery_backup import restore_server_backup
from recovery_cli import prove_workflow_cli
from recovery_evidence import (
    configuration, domain, fixture_request, journal, prove_delivery_isolation, register, verify_domain,
    verify_export, verify_retained,
)
from recovery_support import (
    artifact, database_url, output, port, save, sha256, sql, start, stop, submit,
    wait_http, wait_port,
)


def arguments():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    for path in (args.server, args.worker):
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error("native server and worker must be executable files")
    args.server, args.worker = args.server.resolve(), args.worker.resolve()
    args.evidence = args.evidence.absolute()
    if args.evidence.exists():
        parser.error("evidence directory must be new to prevent evidence replacement")
    return args


def server_config(path, directory, ports, identity):
    fabric, admin, ingress, _ = ports
    text = '\n'.join([
        'roles = ["http-ingress", "admin", "worker", "log-server", "metadata-server"]',
        'node-name = "recovery-' + identity + '"',
        'cluster-name = "recovery-' + identity + '"',
        'auto-provision = true', 'default-num-partitions = 1',
        'default-replication = 1', 'base-dir = ' + json.dumps(str(directory)),
        'listen-mode = "tcp"', 'bind-ip = "127.0.0.1"',
        'bind-port = ' + str(fabric),
        'advertised-address = "http://127.0.0.1:' + str(fabric) + '/"',
        'shutdown-timeout = "10s"', 'disable-telemetry = true',
        'rocksdb-total-memory-size = "256 MiB"',
        'default-journal-retention = "90d"',
        'default-idempotency-retention = "90d"',
        'default-workflow-completion-retention = "180d"',
        'log-format = "json"', 'log-disable-ansi-codes = true',
        '[admin]', 'bind-port = ' + str(admin),
        'advertised-address = "http://127.0.0.1:' + str(admin) + '/"',
        '[ingress]', 'bind-port = ' + str(ingress),
        'advertised-address = "http://127.0.0.1:' + str(ingress) + '/"',
        '[default-retry-policy]', 'max-attempts = 3',
        'on-max-attempts = "pause"', '',
    ])
    path.write_text(text)


class Runtime:
    def __init__(self, args, database, identity):
        self.args, self.database = args, database
        self.server = self.worker = None
        selected = set()
        while len(selected) < 4:
            selected.add(port())
        self.ports = tuple(selected)
        self.admin = "http://127.0.0.1:" + str(self.ports[1])
        self.ingress = "http://127.0.0.1:" + str(self.ports[2])
        self.endpoint = "http://127.0.0.1:" + str(self.ports[3])
        self.root = args.evidence / "artifacts"
        self.root.mkdir(mode=0o700)
        self.config = args.evidence / "restate.toml"
        server_config(self.config, args.evidence / "server-state", self.ports, identity)
        self.environment = dict(os.environ, RUST_LOG="error", NO_PROXY="127.0.0.1,localhost")
        self.environment.pop("DATABASE_URL", None)
        self.environment.pop("TEST_DATABASE_URL", None)
        for name in tuple(self.environment):
            if name.startswith("RESTATE_"):
                self.environment.pop(name)

    def start_server(self):
        self.server = start([str(self.args.server), "--config-file", str(self.config),
            "--no-logo"], self.args.evidence, self.environment,
            self.args.evidence / "server.log")
        wait_port(self.ports[0], self.server)
        wait_http(self.admin, "/health")
        wait_http(self.ingress, "/restate/health")

    def start_worker(self):
        environment = dict(self.environment, DATABASE_URL=self.database)
        self.worker = start([str(self.args.worker), "durable", "serve", "--bind",
            "127.0.0.1:" + str(self.ports[3]), "--artifacts", str(self.root),
            "--published-workbook", str(self.args.evidence / "delivered.xlsx")],
            self.args.evidence, environment, self.args.evidence / "worker.log")
        wait_port(self.ports[3], self.worker)

    def restart_with_pending_export(self, request):
        os.killpg(self.worker.pid, signal.SIGSTOP)
        first_id, first = submit(self.ingress, request)
        second_id, second = submit(self.ingress, request)
        if first_id != second_id:
            raise RuntimeError("idempotent export submissions created different invocations")
        proof = {"invocation_id": first_id, "first": first, "duplicate": second,
            "worker_stopped_during_acceptance": True}
        if not stop(self.server):
            raise RuntimeError("owned Restate server did not shut down gracefully")
        self.server = None
        stop(self.worker, graceful=False)
        self.worker = None
        proof["worker_abruptly_terminated"] = True
        proof["server_gracefully_stopped"] = True
        proof["backup_restore"] = restore_server_backup(
            self.args.evidence, self.config, first_id)
        save(self.args.evidence / "restart-proof.json", proof)
        self.start_server()
        self.start_worker()
        proof["started_from_verified_backup_clone"] = True
        proof["same_worker_artifact_root"] = True
        save(self.args.evidence / "restart-proof.json", proof)
        return first_id

    def cleanup(self):
        result = {}
        for name in ("worker", "server"):
            try:
                result[name + "_graceful"] = stop(getattr(self, name))
            except Exception as error:
                result[name + "_graceful"] = False
                result[name + "_cleanup_error"] = type(error).__name__
        return result


def replay_fixture(runtime, request):
    first_id, first = submit(runtime.ingress, request)
    second_id, second = submit(runtime.ingress, request)
    if first_id != second_id:
        raise RuntimeError("idempotent fixture submissions created different invocations")
    result = output(runtime.ingress, first_id)
    artifact(result["receipt"], runtime.root)
    counts = domain(runtime.database)
    save(runtime.args.evidence / (request["operation_id"] + "-replay.json"), {
        "invocation_id": first_id, "first": first, "duplicate": second,
        "result": result, "domain": counts})
    return first_id, result, counts


def exercise(runtime, identity):
    workflow_cli = prove_workflow_cli(runtime, identity)
    repository = Path(__file__).resolve().parents[2]
    replays = []
    requests = [fixture_request(repository, runtime.root, identity, source)
        for source in ("athleticnet", "milesplit")]
    for request in requests:
        save(runtime.args.evidence / (request["operation_id"] + "-request.json"), request)
        invocation_id, replayed, _ = replay_fixture(runtime, request)
        replays.append((request, invocation_id, replayed))
    counts = domain(runtime.database)
    verify_domain(counts)
    verify_retained(counts, requests, runtime.root)
    export = {"operation_id": "export-" + identity,
        "spec": {"kind": "export", "config": configuration()}}
    save(runtime.args.evidence / "export-request.json", export)
    export_id = runtime.restart_with_pending_export(export)
    exported = output(runtime.ingress, export_id)
    verified = verify_export(exported, runtime.root, runtime.args.evidence / "delivered.xlsx")
    for request, expected_id, expected_output in (*replays, (export, export_id, exported)):
        actual_id, _ = submit(runtime.ingress, request)
        if actual_id != expected_id or output(runtime.ingress, actual_id) != expected_output:
            raise RuntimeError("retained invocation or output changed after restart and replay")
    verify_retained(counts, requests, runtime.root)
    if domain(runtime.database) != counts:
        raise RuntimeError("fixture domain state changed after replay or export restart")
    if verify_export(exported, runtime.root, runtime.args.evidence / "delivered.xlsx") != verified:
        raise RuntimeError("immutable workbook or canonical pointer changed on duplicate export")
    save(runtime.args.evidence / "export-result.json", exported)
    save(runtime.args.evidence / "export-verification.json", verified)
    retained_journal = journal(runtime.admin, export_id, runtime.args.evidence)
    isolation = prove_delivery_isolation(verified, runtime.root, runtime.args.evidence / "delivered.xlsx")
    save(runtime.args.evidence / "delivery-isolation.json", isolation)
    return {"offline_invocation_ids": [item[1] for item in replays], "export_invocation_id": export_id,
        "domain": counts, "export": verified, "workflow_cli": workflow_cli,
        "journal": retained_journal, "delivery_copy_isolation": isolation,
        "backup_restore": json.loads((runtime.args.evidence / "restart-proof.json").read_text())}


def main():
    args = arguments()
    original = os.environ.get("TEST_DATABASE_URL")
    if not original:
        raise RuntimeError("TEST_DATABASE_URL is required; production fallback is forbidden")
    os.umask(0o077)
    args.evidence.mkdir(parents=True, mode=0o700)
    identity = uuid.uuid4().hex
    schema = "restate_e2e_" + identity
    scoped = database_url(original, schema)
    runtime = None
    created = False
    report = {"status": "failed", "schema": schema,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "binary_sha256": {"server": sha256(args.server.read_bytes()),
            "worker": sha256(args.worker.read_bytes())},
        "scope": "isolated PostgreSQL schema, owned native processes, local source fixtures, delivery copies",
        "not_proven": ["mid-chunk process death; covered separately by native export recovery tests",
            "provider outage recovery; this scenario makes no provider HTTP requests"]}
    try:
        sql(original, "CREATE SCHEMA " + schema + ";")
        created = True
        runtime = Runtime(args, scoped, identity)
        runtime.start_server()
        runtime.start_worker()
        save(args.evidence / "registration.json", register(runtime.admin, runtime.endpoint))
        report["proof"] = exercise(runtime, identity)
        report["status"] = "passed"
    except Exception as error:
        text = str(error).replace(original, "[REDACTED]").replace(scoped, "[REDACTED]")
        report["error"] = {"kind": type(error).__name__, "message": text[:2048]}
    finally:
        report["cleanup"] = runtime.cleanup() if runtime else {}
        if created:
            try:
                sql(original, "DROP SCHEMA " + schema + " CASCADE;")
                report["cleanup"]["schema_removed"] = True
            except Exception as error:
                report["cleanup"]["schema_removed"] = False
                report["cleanup"]["schema_cleanup_error"] = type(error).__name__
        if not all(value is True for key, value in report["cleanup"].items()
            if key.endswith("_graceful") or key == "schema_removed"):
            report["status"] = "failed"
        save(args.evidence / "report.json", report)
    print(json.dumps({"status": report["status"], "report": str(args.evidence / "report.json")}))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
