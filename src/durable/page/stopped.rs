use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{FailureClass, FailureRecord, FetchDecision, PageReservation};
use crate::{database::Database, transport::Failure};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const LIMIT: u64 = 8192;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Stop {
    attempt_id: String,
    operation_id: String,
    origin: String,
    status: Option<u16>,
    observed_at_ms: i64,
}

fn load_sync(root: &Path, reservation: &PageReservation) -> Result<Option<Stop>> {
    ensure!(root.is_absolute(), "durable artifact root must be absolute");
    super::binding::validate_digest(&reservation.attempt_id)?;
    let path = root.join(format!("{}.stop", reservation.attempt_id));
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("opening retained access stop"),
    };
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file() && file.metadata()?.len() <= LIMIT,
        "invalid retained access stop file"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= usize::try_from(LIMIT)?,
        "access stop exceeds bound"
    );
    let stop: Stop = serde_json::from_slice(&bytes)?;
    ensure!(
        stop.attempt_id == reservation.attempt_id
            && stop.operation_id == reservation.request.binding.operation_id,
        "retained access stop belongs to another attempt"
    );
    ensure!(
        origin(&stop.origin)? == stop.origin,
        "invalid retained stop origin"
    );
    Ok(Some(stop))
}

fn origin(address: &str) -> Result<String> {
    let url = url::Url::parse(address).context("invalid retained stop address")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "access stop requires an HTTP authority"
    );
    let origin = url.origin().ascii_serialization();
    ensure!(origin.len() <= 2048, "access stop origin exceeds bound");
    Ok(origin)
}

pub(super) async fn load(root: &Path, reservation: &PageReservation) -> Result<Option<Stop>> {
    let root = root.to_owned();
    let reservation = reservation.clone();
    tokio::task::spawn_blocking(move || load_sync(&root, &reservation))
        .await
        .context("reading retained access stop")?
}

pub(super) async fn save(
    root: &Path,
    reservation: &PageReservation,
    failure: &Failure,
    observed_at_ms: i64,
) -> Result<()> {
    ensure!(
        failure.blocked,
        "only access stops can be retained as stops"
    );
    let address = failure.stopped_origin.as_deref().map_or_else(
        || {
            failure
                .capture
                .as_ref()
                .map_or(reservation.request.target.url.as_str(), |c| c.url.as_str())
        },
        |address| address,
    );
    let stop = Stop {
        attempt_id: reservation.attempt_id.clone(),
        operation_id: reservation.request.binding.operation_id.clone(),
        origin: origin(address)?,
        status: failure.capture.as_ref().map(|capture| capture.status),
        observed_at_ms,
    };
    let root = root.to_owned();
    let reservation = reservation.clone();
    tokio::task::spawn_blocking(move || save_sync(&root, &reservation, &stop))
        .await
        .context("writing retained access stop")?
}

fn save_sync(root: &Path, reservation: &PageReservation, stop: &Stop) -> Result<()> {
    ensure!(root.is_absolute(), "durable artifact root must be absolute");
    super::binding::validate_digest(&reservation.attempt_id)?;
    let bytes = serde_json::to_vec(stop)?;
    ensure!(
        bytes.len() <= usize::try_from(LIMIT)?,
        "access stop exceeds bound"
    );
    fs::create_dir_all(root)?;
    File::open(root.parent().context("access stop root has no parent")?)?.sync_all()?;
    let path = root.join(format!("{}.stop", reservation.attempt_id));
    let temporary = root.join(format!(
        ".{}.{}.{}.stop.tmp",
        reservation.attempt_id,
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
        match fs::hard_link(&temporary, path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing =
                    load_sync(root, reservation)?.context("retained stop disappeared")?;
                ensure!(
                    existing.origin == stop.origin && existing.status == stop.status,
                    "access stop differs on replay"
                );
            }
            Err(error) => return Err(error).context("publishing retained access stop"),
        }
        File::open(root)?.sync_all()?;
        Ok(())
    })();
    let cleanup = fs::remove_file(temporary);
    result?;
    cleanup.context("removing staged access stop")
}

pub(super) async fn record(
    database: &Database,
    reservation: &PageReservation,
    stop: &Stop,
) -> Result<FetchDecision> {
    let detail = format!(
        "retained source access stop (HTTP {:?}); human review required",
        stop.status
    );
    let outcome = database
        .durable_fail(
            reservation,
            FailureRecord {
                class: FailureClass::Blocked,
                detail: &detail,
                capture: None,
                stopped_origin: Some(&stop.origin),
                observed_at_ms: stop.observed_at_ms,
            },
        )
        .await?;
    Ok(FetchDecision::Settled { outcome })
}
