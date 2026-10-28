import hashlib
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import backup
from common import MigrationError, pg_env


class BackupBoundaries(unittest.TestCase):
    def test_given_colon_export_name_when_selected_then_mac_incompatible_path_is_refused(self):
        with self.assertRaises(MigrationError):
            backup.safe_name("athletes:2026.xlsx")

    def test_given_numeric_workbook_stem_when_exact_manifest_exists_then_selection_requires_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workbook = root / "athletes-2026.xlsx"
            content = b"original workbook"
            workbook.write_bytes(content)
            workbook.with_suffix(".manifest.json").write_text(json.dumps({
                "format": "track-scrapper-export-v1", "complete": True,
                "parts": [{"file": workbook.name, "bytes": len(content),
                           "sha256": hashlib.sha256(content).hexdigest()}],
            }), encoding="utf-8")
            with self.assertRaises(MigrationError):
                backup.export_sources(root / "missing-exports", [workbook])
            (root / "athletes.manifest.json").write_text(json.dumps({
                "format": "track-scrapper-export-v1", "complete": True,
                "parts": [{"file": "athletes.xlsx", "bytes": 0,
                           "sha256": hashlib.sha256(b"").hexdigest()}],
            }), encoding="utf-8")
            selected = backup.export_sources(
                root / "missing-exports", [workbook, workbook.with_suffix(".manifest.json")]
            )
            self.assertEqual(set(selected), {
                Path("imported/athletes-2026.xlsx"), Path("imported/athletes-2026.manifest.json"),
            })

    def test_given_space_delimited_credentials_when_selected_then_backup_and_restore_refuse(self):
        import restore
        for name in ("my passwords.txt", "my cookies.json", "pg_service", "login data"):
            with self.subTest(name=name):
                with self.assertRaises(MigrationError):
                    backup.safe_name(name)
                with self.assertRaises(MigrationError):
                    restore.relative_path("exports/" + name)

    def test_given_incomplete_numbered_manifest_when_selected_then_export_cannot_become_legacy(self):
        for details in (
            {"complete": False, "parts": []},
            {"complete": True, "parts": []},
            {"complete": True, "parts": [{}]},
            {"complete": True, "parts": [{"file": "other.xlsx"}]},
        ):
            with self.subTest(details=details), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                workbook = root / "athletes-0001.xlsx"
                workbook.write_bytes(b"partial workbook")
                (root / "athletes.manifest.json").write_text(json.dumps({
                    "format": "track-scrapper-export-v1", **details,
                }), encoding="utf-8")
                with self.assertRaises(MigrationError):
                    backup.export_sources(root / "missing-exports", [workbook])

    def test_given_only_one_numbered_part_when_completion_manifest_exists_then_selection_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            parts = []
            for number in (1, 2):
                path = root / f"athletes-{number:04d}.xlsx"
                content = f"workbook {number}".encode()
                path.write_bytes(content)
                parts.append({"file": path.name, "bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()})
            (root / "athletes.manifest.json").write_text(json.dumps({
                "format": "track-scrapper-export-v1", "complete": True, "parts": parts,
            }), encoding="utf-8")
            with self.assertRaises(MigrationError):
                backup.export_sources(root / "missing-exports", [root / parts[0]["file"]])

    def test_given_libpq_service_when_migrating_then_implicit_cluster_selection_is_refused(self):
        with patch.dict(os.environ, {"PGSERVICE": "another-cluster"}):
            with self.assertRaises(MigrationError):
                pg_env()

    def test_given_case_colliding_export_names_when_selected_then_unrestorable_bundle_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            upper = root / "Report.xlsx"
            lower = root / "report.xlsx"
            upper.write_bytes(b"upper")
            lower.write_bytes(b"lower")
            with self.assertRaises(MigrationError):
                backup.export_sources(root / "missing-exports", [upper, lower])

    def test_given_symlink_source_when_selected_then_backup_refuses_the_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "actual.toml"
            target.write_text("states=['WI']\n", encoding="utf-8")
            link = root / "config.toml"
            link.symlink_to(target)
            with self.assertRaises(MigrationError):
                backup.source_path(link)
            self.assertEqual(target.read_text(encoding="utf-8"), "states=['WI']\n")

    def test_given_credentials_in_config_when_selected_then_backup_refuses_without_disclosure(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "config.toml"
            password = "not-for-the-transfer-bundle"
            path.write_text(f"database_url='postgresql://user:{password}@host/database'\n", encoding="utf-8")
            with self.assertRaises(MigrationError) as failure:
                backup.check_config(path)
            self.assertNotIn(password, str(failure.exception))

    def test_given_missing_workbook_part_when_manifest_is_selected_then_export_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            exports = root / "exports"
            exports.mkdir()
            manifest = Path("athletes.manifest.json")
            (exports / manifest).write_text(json.dumps({
                "format": "track-scrapper-export-v1", "complete": True,
                "parts": [{"file": "athletes-0001.xlsx", "bytes": 4, "sha256": hashlib.sha256(b"mark").hexdigest()}],
            }), encoding="utf-8")
            with self.assertRaises(MigrationError):
                backup.check_export_sets(root, {manifest: exports / manifest})
            self.assertFalse((exports / "athletes-0001.xlsx").exists())

    def test_given_same_size_changed_workbook_when_manifest_is_checked_then_corruption_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            exports = root / "exports"
            exports.mkdir()
            workbook = Path("athletes-0001.xlsx")
            manifest = Path("athletes.manifest.json")
            (exports / workbook).write_bytes(b"fake")
            (exports / manifest).write_text(json.dumps({
                "format": "track-scrapper-export-v1", "complete": True,
                "parts": [{"file": workbook.name, "bytes": 4, "sha256": hashlib.sha256(b"mark").hexdigest()}],
            }), encoding="utf-8")
            with self.assertRaises(MigrationError):
                backup.check_export_sets(root, {manifest: exports / manifest, workbook: exports / workbook})
            self.assertEqual((exports / workbook).read_bytes(), b"fake")

    def test_given_misnamed_evidence_when_packaged_then_backup_refuses_unverified_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evidence = root / "source-evidence"
            evidence.mkdir()
            name = hashlib.sha256(b"original public profile").hexdigest() + ".body"
            (evidence / name).write_bytes(b"different public profile")
            staging = root / "staging"
            staging.mkdir()
            with self.assertRaises(MigrationError):
                backup.copy_evidence(evidence, staging)
            self.assertEqual((evidence / name).read_bytes(), b"different public profile")

    def test_given_existing_archive_when_packaged_then_original_archive_is_not_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            staging = root / "payload"
            staging.mkdir()
            (staging / "config.toml").write_text("states=['WI']\n", encoding="utf-8")
            output = root / "existing.tar.gz"
            output.write_bytes(b"keep this archive")
            with self.assertRaises(FileExistsError):
                backup.write_archive(staging, output)
            self.assertEqual(output.read_bytes(), b"keep this archive")

    def test_given_unicode_payload_when_archived_then_paths_bytes_and_private_modes_survive(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            staging = root / "payload"
            staging.mkdir()
            exports = staging / "exports"
            exports.mkdir()
            name = "exports/Wisconsin seniors Ż.xlsx"
            (staging / name).write_bytes(b"workbook bytes\x00\xff")
            output = root / "migration.tar.gz"
            backup.write_archive(staging, output)
            with tarfile.open(output, "r:gz") as archive:
                member = archive.getmember(name)
                self.assertEqual(archive.extractfile(member).read(), b"workbook bytes\x00\xff")
                self.assertEqual(member.mode, 0o600)
                self.assertEqual((member.uid, member.gid), (0, 0))
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
