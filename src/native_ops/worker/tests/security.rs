use super::*;
use clap::Parser;
use std::os::unix::fs::symlink;

#[test]
fn regular_file_boundary_rejects_fifo_without_waiting_for_a_writer() -> Result<()> {
    let fixture = Fixture::new()?;
    let file = files::open_regular(&fixture.binary, true, 65_536)?;
    assert!(rustix::fs::fcntl_getfl(&file)?.contains(rustix::fs::OFlags::NONBLOCK));
    let fifo = fixture.root.path().join("untrusted-fifo");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()?
            .success()
    );
    assert!(files::read_private(&fifo, 65_536).is_err());
    Ok(())
}

#[test]
fn launcher_rejects_private_file_permissions_hash_changes_and_symlinks() -> Result<()> {
    let fixture = Fixture::new()?;
    let receipt = fixture.prepare(&fixture.args())?;
    let config = receipt
        .configuration
        .binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("version"))?
        .join("worker.json");
    fs::set_permissions(&fixture.secrets, fs::Permissions::from_mode(0o644))?;
    assert!(launch::prepare(&config).is_err());
    fs::set_permissions(&fixture.secrets, fs::Permissions::from_mode(0o600))?;
    write_private(&receipt.configuration.binary, b"wrong worker", 0o700)?;
    assert!(launch::prepare(&config).is_err());
    write_private(&receipt.configuration.binary, b"worker-byte-fixture", 0o700)?;
    write_private(&receipt.configuration.launcher, b"wrong launcher", 0o700)?;
    assert!(launch::prepare(&config).is_err());
    write_private(
        &receipt.configuration.launcher,
        b"launcher-byte-fixture",
        0o700,
    )?;
    let link = fixture.root.path().join("config-link");
    symlink(&config, &link)?;
    assert!(launch::prepare(&link).is_err());
    fs::remove_file(&fixture.secrets)?;
    symlink(&config, &fixture.secrets)?;
    assert!(launch::prepare(&config).is_err());
    Ok(())
}

#[test]
fn failed_copy_removes_owned_staging_file_and_preserves_unrelated_existing_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let destination = fixture.root.path().join("deployed");
    let neighbor = fixture.root.path().join("neighbor");
    write_private(&neighbor, b"preserved", 0o600)?;
    assert!(files::executable(&fixture.binary, &destination, &"0".repeat(64)).is_err());
    assert!(!destination.exists());
    assert_eq!(fs::read(neighbor)?, b"preserved");
    assert!(
        !fs::read_dir(fixture.root.path())?
            .collect::<std::io::Result<Vec<_>>>()?
            .iter()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".worker-stage-"))
    );
    Ok(())
}

#[test]
fn invalid_controls_and_malformed_environment_are_rejected_without_secret_diagnostics() -> Result<()>
{
    let fixture = Fixture::new()?;
    let mut args = fixture.args();
    args.runtime = fixture.root.path().join("invalid\u{1}directory");
    assert!(fixture.prepare(&args).is_err());
    assert!(!args.runtime.exists());
    let receipt = fixture.prepare(&fixture.args())?;
    let config = receipt
        .configuration
        .binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("version"))?
        .join("worker.json");
    for invalid in [
        "DATABASE_URL=secret-unquoted",
        "DATABASE_URL=\"\"",
        "DATABASE_URL=\"one\"\nDATABASE_URL=\"two\"",
        "DATABASE_URL=\"private\"\nBAD=\"\\u0000\"",
    ] {
        write_private(&fixture.secrets, invalid.as_bytes(), 0o600)?;
        let error = match launch::prepare(&config) {
            Ok(_) => anyhow::bail!("invalid environment accepted"),
            Err(error) => format!("{error:#}"),
        };
        assert!(!error.contains("secret-unquoted"));
        assert!(!error.contains("private\""));
    }
    let oversized = vec![b' '; 65_537];
    write_private(&fixture.secrets, &oversized, 0o600)?;
    assert!(launch::prepare(&config).is_err());
    let configuration = receipt.configuration;
    assert!(
        plist::render(
            &configuration,
            &config,
            fixture.root.path(),
            "label",
            "invalid\u{1}PATH"
        )
        .is_err()
    );
    Ok(())
}

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    install: WorkerInstallArgs,
}

#[test]
fn installer_rejects_privileged_or_out_of_range_ports_before_preparation() {
    for port in ["0", "1023", "65536"] {
        assert!(
            Arguments::try_parse_from([
                "native-ops",
                "--binary",
                "worker",
                "--runtime",
                "runtime",
                "--secrets",
                "private.env",
                "--port",
                port
            ])
            .is_err()
        );
    }
}
