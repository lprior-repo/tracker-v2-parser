use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::{files, manifest::digest, paths};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    format: String,
    complete: bool,
    parts: Vec<Part>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Part {
    file: String,
    bytes: u64,
    sha256: String,
}

fn parts(path: &Path) -> Result<Option<Vec<Part>>> {
    let value: serde_json::Value = files::load(path, 16 * 1024 * 1024)?;
    if value.get("format").and_then(serde_json::Value::as_str) != Some("track-scrapper-export-v1") {
        return Ok(None);
    }
    let completion: Completion = files::load(path, 16 * 1024 * 1024)?;
    ensure!(
        completion.format == "track-scrapper-export-v1"
            && completion.complete
            && !completion.parts.is_empty(),
        "incomplete workbook export"
    );
    completion
        .parts
        .iter()
        .try_fold(BTreeSet::new(), |mut seen, part| {
            paths::relative(&part.file)?;
            ensure!(
                !part.file.contains('/') && digest(&part.sha256) && seen.insert(&part.file),
                "invalid or duplicate workbook part"
            );
            Ok::<_, anyhow::Error>(seen)
        })?;
    Ok(Some(completion.parts))
}

pub(super) fn collect(root: &Path, imported: &[PathBuf]) -> Result<BTreeMap<PathBuf, PathBuf>> {
    let sources = match fs::symlink_metadata(root) {
        Ok(_) => files::list(&paths::source(root, true)?)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let mut selected = BTreeMap::new();
    let mut portable = BTreeMap::new();
    sources
        .iter()
        .map(|source| Ok((source.strip_prefix(root)?.to_owned(), source.clone())))
        .chain(imported.iter().map(|source| {
            Ok((
                Path::new("imported").join(source.file_name().context("imported export filename")?),
                paths::source(source, false)?,
            ))
        }))
        .try_for_each(|entry: Result<(PathBuf, PathBuf)>| {
            let (relative, source) = entry?;
            let text = relative.to_str().context("export filename must be UTF-8")?;
            paths::relative(text)?;
            paths::portable(text, &mut portable)?;
            let lower = text.to_lowercase();
            ensure!(
                ![
                    ".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".dump", ".sql", ".backup",
                    ".bak"
                ]
                .iter()
                .any(|suffix| lower.ends_with(suffix))
                    && !lower
                        .split(['/', '.', '_', '-'])
                        .any(|part| matches!(part, "backup" | "backups")),
                "old backups are not export payloads"
            );
            ensure!(
                selected.insert(relative, source).is_none(),
                "duplicate export destination"
            );
            Ok::<_, anyhow::Error>(())
        })?;
    let available = selected.values().cloned().collect::<BTreeSet<_>>();
    available
        .iter()
        .filter(|source| {
            source
                .extension()
                .is_some_and(|extension| extension == "xlsx")
        })
        .try_for_each(|source| {
            let stem = source
                .file_stem()
                .and_then(|name| name.to_str())
                .context("UTF-8 workbook name")?;
            let stem = regex::Regex::new(r"-[0-9]{4,}$")?.replace(stem, "");
            [
                source.with_extension("manifest.json"),
                source.with_file_name(format!("{stem}.manifest.json")),
            ]
            .iter()
            .try_for_each(|manifest| {
                if !manifest.try_exists()? {
                    return Ok(());
                }
                paths::source(manifest, false)?;
                if let Some(parts) = parts(manifest)? {
                    let exact = source.with_extension("manifest.json") == *manifest;
                    if exact
                        || parts.iter().any(|part| {
                            source
                                .file_name()
                                .is_some_and(|name| name == part.file.as_str())
                        })
                    {
                        ensure!(
                            available.contains(manifest),
                            "select workbook completion manifest and every part together"
                        );
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
        })?;
    Ok(selected)
}

pub(super) fn validate(root: &Path) -> Result<()> {
    let files = files::list(root)?;
    files
        .iter()
        .filter(|path| {
            path.to_str()
                .is_some_and(|name| name.ends_with(".manifest.json"))
        })
        .try_for_each(|path| {
            let Some(parts) = parts(path)? else {
                return Ok(());
            };
            parts.iter().try_for_each(|part| {
                let location = path.parent().context("manifest parent")?.join(&part.file);
                ensure!(
                    files.contains(&location),
                    "workbook manifest part is missing"
                );
                let fingerprint = files::fingerprint(root, &location)?;
                ensure!(
                    fingerprint.bytes == part.bytes && fingerprint.sha256 == part.sha256,
                    "workbook part differs from completion manifest"
                );
                Ok::<_, anyhow::Error>(())
            })
        })
}
