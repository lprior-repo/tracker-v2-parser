import json
import os
from pathlib import Path
import time

from recovery_support import artifact, http, save, sha256, sql, workbook


def configuration():
    return {
        "acquisition": {"mode": "complete"},
        "states": ["OH", "WI"], "graduation_years": [2027, 2028], "season": 2026,
        "limits": {"requests_per_second": 2.0, "max_pages": 100,
            "max_attempts": 3, "concurrency": 1},
        "sources": {"athleticnet": {"enabled": True, "transport": "http"},
            "milesplit": {"enabled": True, "transport": "http"}},
        "export": {"rows_per_file": 2000, "per_athlete_sheets": False,
            "layout": "single_sheet"},
    }


def fixture_request(repository, root, run_id, source):
    fixtures = {
        "athleticnet": ("athleticnet/bio-28872883.json", "28872883", "WI",
            "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0"),
        "milesplit": ("milesplit/athlete-oh-15005189.html", "15005189", "OH",
            "https://oh.milesplit.com/athletes/15005189-brice-fuller"),
    }
    fixture, athlete, state, url = fixtures[source]
    data = (repository / "fixtures" / fixture).read_bytes()
    inputs = root / "inputs"
    inputs.mkdir(mode=0o700, exist_ok=True)
    path = inputs / (sha256(data) + ".input")
    path.write_bytes(data)
    return {"operation_id": "offline-" + source + "-" + run_id, "spec": {
        "kind": "offline_replay", "config": configuration(),
        "target": {"source": source, "kind": "athlete", "url": url,
            "context": {"state": state, "athlete_id": athlete,
                "sport": "outdoor", "season": 2026}},
        "capture": {"path": str(path), "sha256": sha256(data), "bytes": len(data)}}}


def register(admin, endpoint, deadline=90):
    until = time.monotonic() + deadline
    while time.monotonic() < until:
        try:
            status, value = http(admin, "/deployments", {"uri": endpoint})
            if status in (200, 201):
                return value
            if status not in (429, 502, 503, 504):
                raise RuntimeError("owned worker registration failed with HTTP " + str(status))
        except (OSError, TimeoutError):
            pass
        time.sleep(0.25)
    raise RuntimeError("owned worker registration exceeded bounded deadline")


def domain(database):
    return sql(database, """SELECT json_build_object(
        'athletes', (SELECT count(*) FROM athletes),
        'profiles', (SELECT count(*) FROM athlete_sources),
        'results', (SELECT count(*) FROM results),
        'individual', (SELECT count(*) FROM results WHERE athlete_id IS NOT NULL),
        'relay', (SELECT count(*) FROM results WHERE is_relay),
        'page_receipts', (SELECT count(*) FROM durable_page_receipts),
        'completed_operations', (SELECT count(*) FROM durable_page_operations WHERE state='completed'),
        'source_wire_requests', (SELECT count(*) FROM durable_wire_requests),
        'succeeded_pages', (SELECT count(*) FROM scrape_pages WHERE status='succeeded'),
        'fixture_profile', (SELECT count(*) FROM athlete_sources WHERE source='athleticnet' AND source_id='28872883'),
        'specific_mark', (SELECT count(*) FROM results WHERE source='athleticnet' AND source_result_id='258445410' AND raw_mark='23.33a'),
        'athleticnet_results', (SELECT count(*) FROM results WHERE source='athleticnet'),
        'milesplit_results', (SELECT count(*) FROM results WHERE source='milesplit'),
        'milesplit_prs', (SELECT count(*) FROM results WHERE source='milesplit' AND is_personal_record),
        'unknown_year_profile', (SELECT count(*) FROM athlete_sources WHERE source='athleticnet' AND source_id='28872883' AND graduation_year IS NULL),
        'published_year_profile', (SELECT count(*) FROM athlete_sources WHERE source='milesplit' AND source_id='15005189' AND graduation_year=2028 AND state='OH'),
        'milesplit_specific_prs', (SELECT count(*) FROM results WHERE source='milesplit' AND
            ((sport='outdoor' AND event='1600m' AND raw_mark='4:12.69') OR
             (sport='indoor' AND event='3000m' AND raw_mark='8:26.83') OR
             (sport='xc' AND event='5K' AND raw_mark='14:49.06'))),
        'results_digest', (SELECT md5(coalesce(string_agg(source_result_key || ':' || raw_mark, ',' ORDER BY source_result_key),'')) FROM results),
        'attempts', (SELECT coalesce(sum(attempts),0) FROM scrape_pages),
        'fixture_replay_attempts', (SELECT coalesce(sum(attempts),0) FROM scrape_pages WHERE cursor->>'kind'='athlete'),
        'seeded_legacy_attempts', (SELECT coalesce(sum(attempts),0) FROM scrape_pages WHERE cursor->>'kind'='team'),
        'retained_receipts', (SELECT json_agg(json_build_object(
            'capture',capture,'parsed',parsed,'sha256',receipt_sha256) ORDER BY operation_id) FROM durable_page_receipts)
    );""")


def verify_domain(value):
    expected = {"athletes": 2, "profiles": 2, "results": 81,
        "individual": 69, "relay": 12, "page_receipts": 2,
        "completed_operations": 2, "source_wire_requests": 0,
        "succeeded_pages": 2, "fixture_profile": 1,
        "specific_mark": 1, "attempts": 5, "fixture_replay_attempts": 2,
        "seeded_legacy_attempts": 3, "athleticnet_results": 53,
        "milesplit_results": 28, "milesplit_prs": 28, "unknown_year_profile": 1,
        "published_year_profile": 1, "milesplit_specific_prs": 6}
    for key, count in expected.items():
        if value.get(key) != count:
            raise RuntimeError("isolated fixture assertion failed for " + key
                + ": expected " + str(count) + ", observed " + str(value.get(key)))


def verify_retained(counts, requests, root):
    expected_urls = {request["spec"]["target"]["url"] for request in requests}
    receipts = counts["retained_receipts"]
    if len(receipts) != 2 or {receipt["capture"]["url"] for receipt in receipts} != expected_urls:
        raise RuntimeError("retained receipts do not cover both exact source fixtures")
    for request in requests:
        artifact(request["spec"]["capture"], root)
    references = [receipt[stage]["body"] for receipt in receipts for stage in ("capture", "parsed")]
    for reference in references:
        digest = reference["sha256"]
        if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise RuntimeError("retained page artifact has an invalid SHA-256")
        path = root / (digest + ".blob")
        if path.is_symlink() or not path.is_file():
            raise RuntimeError("retained page artifact is not an owned regular file")
        data = path.read_bytes()
        if sha256(data) != digest or len(data) != reference["bytes"]:
            raise RuntimeError("retained capture or parsed artifact changed after receipt")


def verify_export(result, root, delivery, review_rows):
    _, receipt_bytes = artifact(result["receipt"], root)
    publication = json.loads(receipt_bytes)["publication"]
    path, data = artifact(publication["workbook"], root)
    _, manifest_bytes = artifact(publication["manifest"], root)
    manifest = json.loads(manifest_bytes)
    if manifest["rows"] != 1 or not publication["promoted"]:
        raise RuntimeError("verified publication did not promote exactly one fixture athlete")
    pointer = Path(publication["canonical_pointer"]).resolve()
    if pointer != root.resolve() / "current-export.json":
        raise RuntimeError("canonical export pointer escaped fixed worker root")
    pointer_bytes = pointer.read_bytes()
    if json.loads(pointer_bytes) != publication:
        raise RuntimeError("canonical export pointer does not reference verified publication")
    if result["output"] != str(path):
        raise RuntimeError("operation output differs from verified immutable workbook")
    manifest_delivery = delivery.with_suffix(".manifest.json")
    manifest_path, _ = artifact(publication["manifest"], root)
    for destination, source, expected in (
        (delivery, path, data), (manifest_delivery, manifest_path, manifest_bytes)):
        if destination.is_symlink() or not destination.is_file():
            raise RuntimeError("delivered workbook and manifest must be regular copies")
        if destination.resolve() == source or destination.samefile(source):
            raise RuntimeError("delivered copy must have a different inode from immutable artifact")
        if sha256(destination.read_bytes()) != sha256(expected):
            raise RuntimeError("delivered copy hash differs from verified immutable artifact")
    return {"workbook": workbook(path, review_rows), "delivery": str(delivery), "delivery_kind": "regular_copies",
        "sha256": sha256(data),
        "canonical_pointer_sha256": sha256(pointer_bytes),
        "frozen_manifest_sha256": manifest["frozen_manifest"]["sha256"],
        "publication": publication}


def prove_delivery_isolation(verified, root, delivery):
    publication = verified["publication"]
    proof = {"delivery_kind": "regular_copies", "edited_copies": []}
    for destination, descriptor in (
        (delivery, publication["workbook"]),
        (delivery.with_suffix(".manifest.json"), publication["manifest"])):
        source, original = artifact(descriptor, root)
        if destination.is_symlink() or destination.samefile(source):
            raise RuntimeError("cannot safely edit test delivery because it aliases immutable artifact")
        with destination.open("ab") as handle:
            handle.write(b"\nowned-test-user-edit-isolation-proof\n")
            handle.flush()
            os.fsync(handle.fileno())
        edited_sha256 = sha256(destination.read_bytes())
        if edited_sha256 == sha256(original):
            raise RuntimeError("test delivery edit did not change the user copy")
        artifact(descriptor, root)
        proof["edited_copies"].append({"path": str(destination),
            "edited_sha256": edited_sha256, "immutable_sha256": descriptor["sha256"],
            "immutable_artifact_unchanged": True})
    pointer = Path(publication["canonical_pointer"])
    if sha256(pointer.read_bytes()) != verified["canonical_pointer_sha256"]:
        raise RuntimeError("user delivery edit changed canonical export pointer")
    proof["immutable_workbook_reopened"] = workbook(Path(publication["workbook"]["path"]),
        verified["workbook"]["review"]["checkpoints"])
    proof["canonical_pointer_unchanged"] = True
    return proof


def journal(admin, identity, evidence):
    statements = {
        "invocation": "SELECT * FROM sys_invocation WHERE id='" + identity + "'",
        "journal": "SELECT * FROM sys_journal WHERE id='" + identity + "'",
    }
    result = {}
    for label, statement in statements.items():
        status, value = http(admin, "/query", {"query": statement})
        save(evidence / (label + ".json"), {"status": status, "response": value})
        result[label + "_query_status"] = status
        if label == "journal" and status == 200:
            text = json.dumps(value)
            names = ["freeze-export-input", "read-frozen-chunk-plan",
                "prepare-athlete-chunk-000000", "assemble-workbook",
                "verify-workbook", "publish-verified-workbook", "deliver-verified-workbook",
                "complete-export-receipt"]
            missing = [name for name in names if name not in text]
            if missing:
                raise RuntimeError("retained export journal is missing required stage names")
            result["retained_stage_names"] = names
    result["query_proof"] = "verified" if result["journal_query_status"] == 200 else "unavailable"
    return result
