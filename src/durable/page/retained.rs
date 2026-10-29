use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{CaptureRef, PageReservation};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct Manifest {
    attempt_id: String,
    operation_id: String,
    capture: CaptureRef,
}

fn load_sync(root: &Path, reservation: &PageReservation) -> Result<Option<CaptureRef>> {
    ensure!(root.is_absolute(), "durable artifact root must be absolute");
    super::binding::validate_digest(&reservation.attempt_id)?;
    let path = root.join(format!("{}.capture", reservation.attempt_id));
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("opening durable capture manifest"),
    };
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file() && file.metadata()?.len() <= 32_768,
        "invalid durable capture manifest file"
    );
    let mut bytes = Vec::new();
    file.take(32_769).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 32_768,
        "durable capture manifest exceeds limit"
    );
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.attempt_id == reservation.attempt_id
            && manifest.operation_id == reservation.request.binding.operation_id,
        "retained capture manifest belongs to another attempt"
    );
    Ok(Some(manifest.capture))
}

pub(super) async fn load(root: &Path, reservation: &PageReservation) -> Result<Option<CaptureRef>> {
    let root = root.to_owned();
    let reservation = reservation.clone();
    tokio::task::spawn_blocking(move || load_sync(&root, &reservation))
        .await
        .context("reading retained capture manifest")?
}

pub(super) async fn save(
    root: &Path,
    reservation: &PageReservation,
    capture: &CaptureRef,
) -> Result<()> {
    let root = root.to_owned();
    let reservation = reservation.clone();
    let manifest = Manifest {
        attempt_id: reservation.attempt_id.clone(),
        operation_id: reservation.request.binding.operation_id.clone(),
        capture: capture.clone(),
    };
    tokio::task::spawn_blocking(move || -> Result<()> {
        let bytes = serde_json::to_vec(&manifest)?;
        ensure!(
            bytes.len() <= 32_768,
            "durable capture manifest exceeds limit"
        );
        let path = root.join(format!("{}.capture", manifest.attempt_id));
        let temporary = root.join(format!(
            ".{}.{}.{}.capture.tmp",
            manifest.attempt_id,
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let result = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure!(
                        load_sync(&root, &reservation)?.as_ref() == Some(&manifest.capture),
                        "capture manifest differs on replay"
                    );
                }
                Err(error) => return Err(error).context("publishing durable capture manifest"),
            }
            File::open(&root)?.sync_all()?;
            Ok(())
        })();
        let cleanup = fs::remove_file(&temporary);
        result?;
        cleanup.context("removing staged capture manifest")
    })
    .await
    .context("writing retained capture manifest")?
}
