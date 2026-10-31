use std::{collections::BTreeMap, io::Write, path::Path};

use anyhow::Result;
use sha2::{Digest, Sha256};

use super::{
    archive, database, files,
    manifest::{FORMAT, Inventory, Manifest, PREVIOUS, PREVIOUS_TABLES, TABLES, Table},
    paths, restore,
};

#[path = "tests/admin.rs"]
mod admin;
#[path = "tests/archive_metadata.rs"]
mod archive_metadata;

#[test]
fn given_legacy_v1_archive_when_validated_then_seven_core_tables_remain_the_explicit_scope()
-> Result<()> {
    let root = temporary()?;
    bundle(root.path())?;
    let mut inventory: Inventory =
        files::load(&root.path().join("inventory.json"), super::manifest::SMALL)?;
    inventory.tables.truncate(7);
    std::fs::remove_file(root.path().join("inventory.json"))?;
    files::json(&root.path().join("inventory.json"), &inventory)?;
    let mut manifest: Manifest = files::load(&root.path().join("manifest.json"), 64 * 1024 * 1024)?;
    std::fs::remove_file(root.path().join("manifest.json"))?;
    std::fs::remove_dir(root.path().join("durable-artifacts"))?;
    manifest.format = super::manifest::LEGACY.into();
    manifest.artifacts_dir = None;
    manifest.files = files::list(root.path())?
        .iter()
        .map(|path| files::fingerprint(root.path(), path))
        .collect::<Result<Vec<_>>>()?;
    files::json(&root.path().join("manifest.json"), &manifest)?;
    let work = temporary()?;
    let archive = work.path().join("legacy.tar.gz");
    archive::write(root.path(), &archive)?;
    let restored = temporary()?;
    let (legacy, actual) = archive::unpack(&archive, restored.path())?;
    assert_eq!(legacy.format, super::manifest::LEGACY);
    assert!(legacy.artifacts_dir.is_none());
    assert_eq!(actual, inventory);
    assert_eq!(actual.tables.len(), 7);
    Ok(())
}

fn temporary() -> Result<tempfile::TempDir> {
    Ok(tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?)
}

fn bundle(root: &Path) -> Result<()> {
    ["evidence", "exports", "durable-artifacts"]
        .iter()
        .try_for_each(|name| files::directory(&root.join(name)))?;
    files::create(&root.join("config.toml"))?.write_all(b"states=['WI']\n")?;
    files::create(&root.join("database.dump"))?.write_all(b"PGDMPfixture")?;
    let inventory = Inventory {
        tables: TABLES
            .iter()
            .map(|(name, _)| Table {
                name: (*name).into(),
                rows: 0,
                sha256: format!("{:x}", Sha256::digest([])),
            })
            .collect(),
    };
    files::json(&root.join("inventory.json"), &inventory)?;
    let content = b"retained fixture";
    files::create(
        &root
            .join("evidence")
            .join(format!("{:x}.body", Sha256::digest(content))),
    )?
    .write_all(content)?;
    let listed = files::list(root)?
        .iter()
        .map(|path| files::fingerprint(root, path))
        .collect::<Result<Vec<_>>>()?;
    let manifest = Manifest {
        format: FORMAT.into(),
        created_at_utc: "2026-10-29T00:00:00Z".into(),
        source_postgres_major: 18,
        source_postgres_version: "18.6".into(),
        database_dump: "database.dump".into(),
        config: "config.toml".into(),
        evidence_dir: "evidence".into(),
        exports_dir: "exports".into(),
        inventory: "inventory.json".into(),
        artifacts_dir: Some("durable-artifacts".into()),
        required_artifacts: Vec::new(),
        files: listed,
    };
    files::json(&root.join("manifest.json"), &manifest)
}

#[test]
fn given_complete_bundle_when_native_archive_roundtrips_then_all_fourteen_fingerprints_survive()
-> Result<()> {
    let source = temporary()?;
    bundle(source.path())?;
    let work = temporary()?;
    let archive = work.path().join("valid.tar.gz");
    archive::write(source.path(), &archive)?;
    let target = temporary()?;
    let (manifest, inventory) = archive::unpack(&archive, target.path())?;
    assert_eq!(manifest.format, FORMAT);
    assert_eq!(inventory.tables.len(), 14);
    assert!(
        inventory
            .tables
            .iter()
            .any(|table| table.name == "durable_wire_requests")
    );
    assert_eq!(
        inventory.tables.last().map(|table| table.name.as_str()),
        Some("reviewed_result_links")
    );
    assert!(archive::write(source.path(), &archive).is_err());
    Ok(())
}

#[test]
fn given_previous_v2_bundle_when_native_archive_roundtrips_then_exact_thirteen_table_scope_survives()
-> Result<()> {
    let source = temporary()?;
    bundle(source.path())?;
    let mut inventory: Inventory = files::load(
        &source.path().join("inventory.json"),
        super::manifest::SMALL,
    )?;
    inventory.tables.truncate(PREVIOUS_TABLES.len());
    std::fs::remove_file(source.path().join("inventory.json"))?;
    files::json(&source.path().join("inventory.json"), &inventory)?;
    let mut manifest: Manifest =
        files::load(&source.path().join("manifest.json"), 64 * 1024 * 1024)?;
    manifest.format = PREVIOUS.into();
    std::fs::remove_file(source.path().join("manifest.json"))?;
    manifest.files = files::list(source.path())?
        .iter()
        .map(|path| files::fingerprint(source.path(), path))
        .collect::<Result<Vec<_>>>()?;
    files::json(&source.path().join("manifest.json"), &manifest)?;
    let work = temporary()?;
    let archive = work.path().join("previous-v2.tar.gz");
    archive::write(source.path(), &archive)?;
    let target = temporary()?;
    let (manifest, actual) = archive::unpack(&archive, target.path())?;
    assert_eq!(manifest.format, PREVIOUS);
    assert_eq!(actual.tables.len(), 13);
    assert_eq!(
        actual
            .tables
            .iter()
            .map(|table| table.name.as_str())
            .collect::<Vec<_>>(),
        PREVIOUS_TABLES
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn given_current_v3_bundle_missing_audited_table_when_restored_then_validation_fails() -> Result<()>
{
    let source = temporary()?;
    bundle(source.path())?;
    let mut inventory: Inventory = files::load(
        &source.path().join("inventory.json"),
        super::manifest::SMALL,
    )?;
    inventory.tables.pop();
    std::fs::remove_file(source.path().join("inventory.json"))?;
    files::json(&source.path().join("inventory.json"), &inventory)?;
    let mut manifest: Manifest =
        files::load(&source.path().join("manifest.json"), 64 * 1024 * 1024)?;
    std::fs::remove_file(source.path().join("manifest.json"))?;
    manifest.files = files::list(source.path())?
        .iter()
        .map(|path| files::fingerprint(source.path(), path))
        .collect::<Result<Vec<_>>>()?;
    files::json(&source.path().join("manifest.json"), &manifest)?;
    let work = temporary()?;
    let archive = work.path().join("missing-v3-table.tar.gz");
    archive::write(source.path(), &archive)?;
    let target = temporary()?;
    let error =
        archive::unpack(&archive, target.path()).expect_err("v3 must include audited table");
    assert!(format!("{error:#}").contains("table inventory"));
    Ok(())
}

#[test]
fn given_unsafe_paths_when_validated_then_traversal_credentials_and_unicode_collisions_are_rejected()
-> Result<()> {
    for name in [
        "../config.toml",
        "/tmp/file",
        "exports/../../file",
        "exports/.env",
        "exports/credentials.json",
        "exports/ſecret.txt",
        "exports/test\\file",
        "exports/a\0b",
        "exports/id_ed25519",
    ] {
        assert!(paths::payload(name, false).is_err(), "accepted{name:?}");
    }
    let mut seen = BTreeMap::new();
    paths::portable("exports/Straße.xlsx", &mut seen)?;
    assert!(paths::portable("exports/STRASSE.xlsx", &mut seen).is_err());
    let mut seen = BTreeMap::new();
    paths::portable("exports/Å.xlsx", &mut seen)?;
    assert!(paths::portable("exports/A\u{30a}.xlsx", &mut seen).is_err());
    assert!(paths::database("postgres://private").is_err());
    assert!(paths::database("valid_2027").is_ok());
    Ok(())
}

fn crafted(path: &Path, name: &str, kind: u8, body: &[u8], declared: u64) -> Result<()> {
    let compressed =
        flate2::GzBuilder::new().write(files::create(path)?, flate2::Compression::default());
    let mut output = compressed;
    let mut header = if kind == b'S' {
        tar::Header::new_gnu()
    } else {
        tar::Header::new_ustar()
    };
    header.set_path("placeholder")?;
    header.set_size(declared);
    if kind == b'S' {
        header
            .as_gnu_mut()
            .expect("GNU sparse fixture")
            .set_real_size(0);
    }
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::new(kind));
    let field = header
        .as_mut_bytes()
        .get_mut(..100)
        .expect("tar path field");
    field.fill(0);
    field
        .get_mut(..name.len())
        .expect("fixture short name")
        .copy_from_slice(name.as_bytes());
    header.set_cksum();
    output.write_all(header.as_bytes())?;
    output.write_all(body)?;
    output.write_all(&[0; 2048])?;
    output.finish()?.sync_all()?;
    Ok(())
}

#[test]
fn given_link_or_hostile_tar_header_when_restoring_then_no_payload_escapes_staging() -> Result<()> {
    let work = temporary()?;
    for (index, (name, kind)) in [
        ("../escape", b'0'),
        ("/tmp/escape", b'0'),
        ("exports/link", b'2'),
        ("exports/hard", b'1'),
        ("exports/device", b'3'),
        ("exports/sparse", b'S'),
        ("exports/long", b'L'),
    ]
    .iter()
    .enumerate()
    {
        let archive = work.path().join(format!("invalid-{index}.tar.gz"));
        crafted(&archive, name, *kind, b"", 0)?;
        let target = temporary()?;
        let error = archive::unpack(&archive, target.path()).expect_err("hostile header");
        let message = format!("{error:#}");
        assert!(
            message.contains("path") || message.contains("forbidden"),
            "{message}"
        );
        assert!(files::list(target.path())?.is_empty());
    }
    Ok(())
}

#[test]
fn given_oversized_or_tampered_payload_when_restoring_then_verification_fails_closed() -> Result<()>
{
    let work = temporary()?;
    let archive = work.path().join("oversized.tar.gz");
    crafted(
        &archive,
        "config.toml",
        b'0',
        b"",
        super::manifest::SMALL + 1,
    )?;
    let target = temporary()?;
    assert!(
        format!(
            "{:#}",
            archive::unpack(&archive, target.path()).expect_err("size bound")
        )
        .contains("limit")
    );
    let source = temporary()?;
    bundle(source.path())?;
    std::fs::write(source.path().join("config.toml"), b"states=['OH']\n")?;
    let archive = work.path().join("tampered.tar.gz");
    archive::write(source.path(), &archive)?;
    let target = temporary()?;
    assert!(
        format!(
            "{:#}",
            archive::unpack(&archive, target.path()).expect_err("hash mismatch")
        )
        .contains("differ")
    );
    Ok(())
}

#[test]
fn given_symlink_or_secret_configuration_when_backing_up_then_credentials_are_not_selected()
-> Result<()> {
    let root = temporary()?;
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "database_url='postgresql://someone:secret@localhost/db'\n",
    )?;
    assert!(paths::config(&config).is_err());
    std::fs::write(&config, "\"ſecret\"='value'\n")?;
    assert!(paths::config(&config).is_err());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&config, &link)?;
    assert!(files::copy(&link, &root.path().join("copy")).is_err());
    assert!(!root.path().join("copy").exists());
    assert!(database::restore_version(18, 17, 18).is_err());
    assert!(database::restore_version(18, 19, 19).is_ok());
    assert!(database::restore_version(12, 18, 18).is_err());
    let encoded = format!(
        "DATABASE_URL={}\n",
        restore::dotenv("postgresql://name\\\"$value@localhost/db")
    );
    let value =
        dotenvy::from_read_iter(encoded.as_bytes()).collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(value[0].1, "postgresql://name\\\"$value@localhost/db");
    let private = restore::private_env("postgresql://name\\\"$value@localhost/db")?;
    let decoded: String = serde_json::from_str(
        private
            .trim()
            .strip_prefix("DATABASE_URL=")
            .expect("private key"),
    )?;
    assert_eq!(decoded, "postgresql://name\\\"$value@localhost/db");
    Ok(())
}
