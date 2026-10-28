import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

import backup
import restore
from common import FORMAT, TABLES, MigrationError


def make_archive(path, entries):
    with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        for name, content in entries:
            member = tarfile.TarInfo(name)
            if content is None:
                member.type = tarfile.DIRTYPE
                member.mode = 0o700
                archive.addfile(member)
            else:
                member.size = len(content)
                member.mode = 0o600
                archive.addfile(member, io.BytesIO(content))


def valid_entries(extra_payload=None):
    payload = {
        "database.dump": b"PGDMParchive validation fixture",
        "config.toml": b"states=['WI']\ngraduation_years=[2027]\nseason=2026\n",
        "inventory.json": json.dumps({"tables": [
            {"name": name, "rows": 0, "sha256": hashlib.sha256(b"").hexdigest()} for name in TABLES
        ]}).encode(),
        "exports/athletes.xlsx": b"original workbook bytes",
    }
    if extra_payload:
        payload.update(extra_payload)
    manifest = {
        "format": FORMAT,
        "created_at_utc": "2026-10-28T00:00:00Z",
        "source_postgres_major": 18,
        "source_postgres_version": "18.6",
        "database_dump": "database.dump",
        "config": "config.toml",
        "evidence_dir": "evidence",
        "exports_dir": "exports",
        "inventory": "inventory.json",
        "files": [{"path": name, "bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}
                  for name, content in sorted(payload.items())],
    }
    return [("evidence", None), ("exports", None), *sorted(payload.items()),
            ("manifest.json", json.dumps(manifest).encode())]


class RestoreArchiveBoundaries(unittest.TestCase):
    def test_given_oversized_configuration_when_packaged_or_restored_then_both_sides_refuse(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "config.toml"
            prefix = b"states=['WI']\n"
            path.write_bytes(prefix + b" " * (1024 * 1024 - len(prefix)))
            for checker in (backup.check_config, restore.validate_config):
                checker(path)
            with path.open("ab") as stream:
                stream.write(b" ")
            for checker in (backup.check_config, restore.validate_config):
                with self.subTest(checker=checker.__module__):
                    with self.assertRaises(MigrationError):
                        checker(path)

    def test_given_unicode_and_long_export_paths_when_producer_archive_is_restored_then_pax_paths_survive(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            names = ["exports/Ż seniors.xlsx", "exports/" + "recruiting-" * 12 + ".xlsx"]
            for name, content in valid_entries({name: b"portable workbook" for name in names}):
                path = source / name
                if content is None:
                    path.mkdir(parents=True, exist_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(content)
            archive = root / "portable.tar.gz"
            backup.write_archive(source, archive)
            destination = root / "restored"
            destination.mkdir()
            restore.unpack(archive, destination)
            for name in names:
                self.assertEqual((destination / name).read_bytes(), b"portable workbook")
                self.assertEqual((destination / name).stat().st_mode & 0o777, 0o600)

    def test_given_traversal_member_when_unpacked_then_no_file_escapes_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "bad.tar.gz"
            make_archive(archive, [("../outside.txt", b"must not escape")])
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertFalse((root / "outside.txt").exists())

    def test_given_symlink_member_when_unpacked_then_no_symlink_is_created(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "bad.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                member = tarfile.TarInfo("exports/link.xlsx")
                member.type = tarfile.SYMTYPE
                member.linkname = str(root / "outside.xlsx")
                stream.addfile(member)
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertFalse((stage / "exports/link.xlsx").is_symlink())
            self.assertFalse((root / "outside.xlsx").exists())

    def test_given_duplicate_path_when_unpacked_then_first_payload_cannot_be_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "duplicate.tar.gz"
            make_archive(archive, [("exports/report.xlsx", b"first"), ("exports/report.xlsx", b"second")])
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertEqual((stage / "exports/report.xlsx").read_bytes(), b"first")

    def test_given_case_colliding_paths_when_unpacked_then_bundle_is_not_mac_portable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "collision.tar.gz"
            make_archive(archive, [("exports/Team/report.xlsx", b"original"), ("exports/team/other.xlsx", b"collision")])
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertEqual((stage / "exports/Team/report.xlsx").read_bytes(), b"original")
            self.assertFalse((stage / "exports/team/other.xlsx").exists())

    def test_given_checksum_mismatch_when_unpacked_then_corrupted_bundle_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "corrupted.tar.gz"
            entries = [(name, b"changed! workbook bytes" if name == "exports/athletes.xlsx" else content)
                       for name, content in valid_entries()]
            make_archive(archive, entries)
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertFalse((stage / ".env").exists())

    def test_given_unlisted_regular_file_when_unpacked_then_manifest_cannot_hide_extra_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "extra.tar.gz"
            make_archive(archive, valid_entries() + [("exports/unlisted.xlsx", b"unlisted")])
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertFalse((stage / ".env").exists())

    def test_given_env_file_in_archive_when_unpacked_then_source_credentials_are_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "credentials.tar.gz"
            make_archive(archive, [("exports/.env", b"DATABASE_URL=do-not-import")])
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertFalse((stage / "exports/.env").exists())

    def test_given_concatenated_gzip_payload_when_unpacked_then_hidden_trailer_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            archive = root / "trailer.tar.gz"
            make_archive(archive, valid_entries())
            with archive.open("ab") as stream:
                stream.write(gzip.compress(b"unexpected non-tar payload"))
            with self.assertRaises(MigrationError):
                restore.unpack(archive, stage)
            self.assertFalse((stage / ".env").exists())


if __name__ == "__main__":
    unittest.main()
