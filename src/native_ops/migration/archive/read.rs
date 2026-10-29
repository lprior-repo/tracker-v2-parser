use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufReader, Read},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use flate2::bufread::GzDecoder;

use super::super::{
    files,
    manifest::{Fingerprint, MAX_BYTES, MAX_FILES, SMALL},
    paths,
};

struct Scope {
    seen: BTreeSet<String>,
    portable: BTreeMap<String, String>,
    actual: Vec<Fingerprint>,
    pending: Option<BTreeMap<String, String>>,
    payload_bytes: u64,
    metadata_bytes: u64,
    headers: usize,
}

pub fn unpack(archive: &Path, root: &Path) -> Result<(super::Manifest, super::Inventory)> {
    let decoder = GzDecoder::new(BufReader::new(files::open(archive)?));
    let mut archive = tar::Archive::new(decoder);
    let scope = Scope {
        seen: BTreeSet::new(),
        portable: BTreeMap::new(),
        actual: Vec::new(),
        pending: None,
        payload_bytes: 0,
        metadata_bytes: 0,
        headers: 0,
    };
    let scope = archive
        .entries()?
        .raw(true)
        .try_fold(scope, |mut scope, entry| {
            let mut entry = entry?;
            scope.headers = scope
                .headers
                .checked_add(1)
                .context("header count overflow")?;
            ensure!(
                scope.headers <= MAX_FILES.checked_mul(2).context("header bound overflow")?,
                "archive has too many headers"
            );
            scope.metadata_bytes = scope
                .metadata_bytes
                .checked_add(512)
                .context("metadata overflow")?;
            ensure!(
                scope.metadata_bytes <= 128 * 1024 * 1024,
                "archive metadata exceeds limit"
            );
            let kind = entry.header().entry_type().as_byte();
            ensure!(
                matches!(kind, 0 | b'0' | b'5' | b'x'),
                "archive links, devices, sparse files and unsupported headers are forbidden"
            );
            let size = entry.header().entry_size()?;
            if kind == b'x' {
                ensure!(
                    scope.pending.is_none() && size <= 65_536,
                    "nested or oversized PAX metadata"
                );
                let mut bytes = Vec::new();
                (&mut entry).take(65_537).read_to_end(&mut bytes)?;
                scope.metadata_bytes = scope
                    .metadata_bytes
                    .checked_add(size)
                    .context("metadata overflow")?;
                scope.pending = Some(pax(&bytes)?);
                return Ok(scope);
            }
            ensure!(
                matches!(kind, 0 | b'0' | b'5'),
                "archive links, devices, sparse files and unsupported headers are forbidden"
            );
            let header_path = entry.header().path_bytes();
            let raw_name = std::str::from_utf8(&header_path)?;
            let extensions = scope.pending.take();
            let name = extensions
                .as_ref()
                .and_then(|values| values.get("path"))
                .map_or(raw_name, String::as_str);
            let owned_name = if kind == b'5' {
                name.trim_end_matches('/').to_owned()
            } else {
                name.to_owned()
            };
            let name = owned_name.as_str();
            scope.metadata_bytes = scope
                .metadata_bytes
                .checked_add(u64::try_from(name.len())?)
                .context("metadata overflow")?;
            ensure!(
                scope.metadata_bytes <= 128 * 1024 * 1024,
                "archive metadata exceeds limit"
            );
            paths::payload(name, kind == b'5')?;
            ensure!(
                scope.seen.len() < MAX_FILES && scope.seen.insert(name.to_owned()),
                "duplicate archive entry or too many members"
            );
            paths::portable(name, &mut scope.portable)?;
            let destination = root.join(name);
            if kind == b'5' {
                ensure!(size == 0, "archive directory has payload bytes");
                files::directory(&destination)?;
                return Ok(scope);
            }
            copy_payload(&mut scope, &mut entry, root, name, size)?;
            Ok::<_, anyhow::Error>(scope)
        })?;
    ensure!(
        scope.pending.is_none(),
        "PAX metadata lacks following payload"
    );
    let mut decoder = archive.into_inner();
    zeros(&mut decoder)?;
    zeros(&mut decoder.into_inner())?;
    super::validate(root, scope.actual)
}

fn copy_payload(
    scope: &mut Scope,
    entry: &mut impl Read,
    root: &Path,
    name: &str,
    size: u64,
) -> Result<()> {
    let destination = root.join(name);
    let limit = match name {
        "manifest.json" => 64 * 1024 * 1024,
        "config.toml" | "scrape.toml" | "inventory.json" => SMALL,
        _ => MAX_BYTES,
    };
    scope.payload_bytes = scope
        .payload_bytes
        .checked_add(size)
        .context("payload size overflow")?;
    ensure!(
        size <= limit && scope.payload_bytes <= MAX_BYTES,
        "archive exceeds payload limits"
    );
    files::directory(destination.parent().context("archive payload parent")?)?;
    let mut output = files::create(&destination)?;
    let copied = std::io::copy(entry, &mut output)?;
    ensure!(copied == size, "archive payload is truncated");
    output.sync_all()?;
    let fingerprint = files::fingerprint(root, &destination)?;
    if name.starts_with("evidence/")
        || (name.starts_with("durable-artifacts/")
            && destination
                .extension()
                .is_some_and(|extension| extension == "blob" || extension == "input"))
    {
        ensure!(
            destination.file_stem().and_then(|name| name.to_str())
                == Some(fingerprint.sha256.as_str()),
            "artifact filename differs from content SHA256"
        );
    }
    if name != "manifest.json" {
        scope.actual.push(fingerprint);
    }
    Ok(())
}

fn zeros(input: &mut impl Read) -> Result<()> {
    let mut trailing = Vec::new();
    input
        .take(SMALL.checked_add(1).context("trailer bound overflow")?)
        .read_to_end(&mut trailing)?;
    ensure!(
        u64::try_from(trailing.len())? <= SMALL && trailing.iter().all(|byte| *byte == 0),
        "archive has unexpected trailing data"
    );
    Ok(())
}

fn pax(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= 65_536,
        "invalid PAX metadata size"
    );
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .try_fold(BTreeMap::new(), |mut values, record| {
            let record = std::str::from_utf8(record)?;
            let (length, entry) = record.split_once(' ').context("PAX record has no length")?;
            ensure!(
                length.len() <= 10
                    && length.bytes().all(|byte| byte.is_ascii_digit())
                    && length.parse::<usize>()? == record.len(),
                "invalid PAX record length"
            );
            let entry = entry
                .strip_suffix('\n')
                .context("PAX record is truncated")?;
            let (key, value) = entry
                .split_once('=')
                .context("PAX record has no assignment")?;
            ensure!(
                ["path", "mtime", "atime", "ctime"].contains(&key)
                    && values.insert(key.to_owned(), value.to_owned()).is_none(),
                "unsupported or duplicate PAX metadata"
            );
            Ok(values)
        })
}
