mod security;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

use super::{WorkerInstallArgs, files, install, launch, model, plist};

struct Fixture {
    root: TempDir,
    binary: PathBuf,
    launcher: PathBuf,
    secrets: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempdir()?;
        let binary = root.path().join("worker-original");
        let launcher = root.path().join("launcher-original");
        let secrets = root.path().join("private.env");
        write_private(&binary, b"worker-byte-fixture", 0o700)?;
        write_private(&launcher, b"launcher-byte-fixture", 0o700)?;
        write_private(&secrets, b"DATABASE_URL=\"postgres://fixture-only/private\"\nTEST_VALUE=\"a=b\\nquoted\\\"value\"\n", 0o600)?;
        Ok(Self {
            root,
            binary,
            launcher,
            secrets,
        })
    }

    fn args(&self) -> WorkerInstallArgs {
        WorkerInstallArgs {
            binary: self.binary.clone(),
            runtime: self.root.path().join("Runtime & <'\">"),
            secrets: self.secrets.clone(),
            port: 9090,
            activate: false,
            published_workbook: Some(self.root.path().join("Current & <'\">.xlsx")),
            launch_agents: None,
        }
    }

    fn prepare(&self, args: &WorkerInstallArgs) -> Result<model::Receipt> {
        install::prepare(args, &self.launcher, &self.root.path().join("agents"))
    }
}

fn write_private(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[test]
fn preparation_copies_both_verified_executables_and_preserves_metadata_without_activation()
-> Result<()> {
    let fixture = Fixture::new()?;
    let args = fixture.args();
    let receipt = fixture.prepare(&args)?;
    assert!(!receipt.activated);
    assert_eq!(receipt.configuration.bind, "127.0.0.1:9090");
    assert_eq!(
        files::digest(&receipt.configuration.binary)?,
        files::digest(&fixture.binary)?
    );
    assert_eq!(
        files::digest(&receipt.configuration.launcher)?,
        files::digest(&fixture.launcher)?
    );
    assert_ne!(receipt.configuration.binary, fixture.binary);
    assert_ne!(receipt.configuration.launcher, fixture.launcher);
    let version = receipt
        .configuration
        .binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("version"))?;
    assert_eq!(
        version.file_name().and_then(|name| name.to_str()),
        Some(
            model::deployment_digest(
                &receipt.configuration.binary_sha256,
                &receipt.configuration.launcher_sha256
            )
            .as_str()
        )
    );
    assert!(!version.join("launch.py").exists());
    let json: Value = serde_json::to_value(&receipt)?;
    for field in [
        "binary",
        "binary_sha256",
        "secrets",
        "artifacts",
        "bind",
        "published_workbook",
        "label",
        "plist",
        "activated",
    ] {
        assert!(json.get(field).is_some(), "missing {field}");
    }
    for path in [
        &receipt.configuration.binary,
        &receipt.configuration.launcher,
        &version.join("worker.json"),
        &receipt.plist,
    ] {
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o077, 0);
    }
    assert_eq!(fs::metadata(version)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(serde_json::to_value(fixture.prepare(&args)?)?, json);
    Ok(())
}

#[test]
fn launcher_upgrade_uses_a_new_immutable_version_without_changing_the_old_deployment() -> Result<()>
{
    let fixture = Fixture::new()?;
    let args = fixture.args();
    let old = fixture.prepare(&args)?;
    let config = old
        .configuration
        .binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("version"))?
        .join("worker.json");
    let old_configuration = fs::read(&config)?;
    let old_plist = fs::read(&old.plist)?;
    write_private(&fixture.launcher, b"new launcher version", 0o700)?;
    let new = fixture.prepare(&args)?;
    assert_ne!(new.configuration.binary, old.configuration.binary);
    assert_ne!(new.label, old.label);
    assert_ne!(new.plist, old.plist);
    assert_eq!(
        new.configuration.binary_sha256,
        old.configuration.binary_sha256
    );
    assert_eq!(fs::read(config)?, old_configuration);
    assert_eq!(fs::read(old.plist)?, old_plist);
    assert_eq!(
        fs::read(old.configuration.launcher)?,
        b"launcher-byte-fixture"
    );
    Ok(())
}

#[test]
fn launchagent_escapes_xml_and_invokes_rust_launcher_with_exact_config_path() -> Result<()> {
    let fixture = Fixture::new()?;
    let receipt = fixture.prepare(&fixture.args())?;
    let body = fs::read_to_string(&receipt.plist)?;
    assert!(body.contains("Runtime &amp; &lt;&apos;&quot;&gt;"));
    assert!(body.contains("<string>worker-launch</string><string>--config</string>"));
    assert!(!body.contains("python"));
    assert!(!body.contains("postgres://fixture-only/private"));
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&receipt.plist)
            .status()?;
        assert!(status.success());
    }
    let config = receipt
        .configuration
        .binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("version"))?
        .join("worker.json");
    let prepared = launch::prepare(&config)?;
    let command = launch::command(&prepared);
    assert_eq!(
        command.get_program(),
        receipt.configuration.binary.as_os_str()
    );
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        vec![
            "durable",
            "serve",
            "--bind",
            "127.0.0.1:9090",
            "--artifacts",
            receipt
                .configuration
                .artifacts
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("artifact path"))?,
            "--published-workbook",
            receipt
                .configuration
                .published_workbook
                .as_ref()
                .and_then(|path| path.to_str())
                .ok_or_else(|| anyhow::anyhow!("workbook path"))?
        ]
    );
    assert_eq!(
        prepared.environment.get("TEST_VALUE").map(String::as_str),
        Some("a=b\nquoted\"value")
    );
    Ok(())
}

#[test]
fn changed_config_plist_worker_or_launcher_cannot_overwrite_existing_deployment() -> Result<()> {
    for change in ["config", "plist", "worker", "launcher"] {
        let fixture = Fixture::new()?;
        let args = fixture.args();
        let receipt = fixture.prepare(&args)?;
        let version = receipt
            .configuration
            .binary
            .parent()
            .ok_or_else(|| anyhow::anyhow!("version"))?;
        let path = match change {
            "config" => version.join("worker.json"),
            "plist" => receipt.plist.clone(),
            "worker" => receipt.configuration.binary.clone(),
            _ => receipt.configuration.launcher.clone(),
        };
        let original = fs::read(&path)?;
        write_private(
            &path,
            b"corrupted immutable bytes",
            if matches!(change, "worker" | "launcher") {
                0o700
            } else {
                0o600
            },
        )?;
        assert!(fixture.prepare(&args).is_err(), "{change}");
        assert_eq!(fs::read(&path)?, b"corrupted immutable bytes");
        assert_ne!(fs::read(&path)?, original);
    }
    let fixture = Fixture::new()?;
    let mut args = fixture.args();
    let receipt = fixture.prepare(&args)?;
    let before = fs::read(
        receipt
            .configuration
            .binary
            .parent()
            .ok_or_else(|| anyhow::anyhow!("version"))?
            .join("worker.json"),
    )?;
    args.port = 9091;
    assert!(fixture.prepare(&args).is_err());
    assert_eq!(
        fs::read(
            receipt
                .configuration
                .binary
                .parent()
                .ok_or_else(|| anyhow::anyhow!("version"))?
                .join("worker.json")
        )?,
        before
    );
    Ok(())
}
