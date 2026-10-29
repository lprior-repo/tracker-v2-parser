use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use super::manifest::digest;

pub(super) fn database(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 63
            && value
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte.is_ascii_alphabetic()
                    || byte == b'_'
                    || (index > 0 && (byte.is_ascii_digit() || byte == b'-'))),
        "database must be an ASCII name, not a connection URL"
    );
    Ok(())
}

pub(super) fn credential(name: &str) -> bool {
    let lower = name.case_fold().collect::<String>();
    lower.split(['.', '_', ' ', '-']).any(|word| {
        matches!(
            word,
            "credential"
                | "credentials"
                | "secret"
                | "secrets"
                | "token"
                | "tokens"
                | "password"
                | "passwords"
                | "passwd"
                | "pgpass"
                | "cookie"
                | "cookies"
                | "browser"
                | "profile"
                | "profiles"
        )
    }) || [".pem", ".key", ".p12", ".pfx"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
        || lower.contains("id_rsa")
        || lower.contains("id_ed25519")
        || lower.contains("pg_service")
        || lower.contains("login data")
}

pub(super) fn relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 4096
            && !value.contains('\\')
            && !value.chars().any(char::is_control),
        "invalid archive path"
    );
    ensure!(
        value.split('/').all(|part| !part.is_empty()
            && !part.starts_with('.')
            && !part.contains(':')
            && !credential(part)),
        "hidden, traversal or credential-like archive path"
    );
    Ok(())
}

pub(super) fn payload(value: &str, directory: bool) -> Result<()> {
    relative(value)?;
    let parts = value.split('/').collect::<Vec<_>>();
    let root = parts.first().copied().context("empty archive path")?;
    if directory {
        let operation = parts.get(1).is_some_and(|name| {
            *name == "inputs" || name.strip_prefix("operation-").is_some_and(digest)
        });
        ensure!(
            root == "exports"
                || (root == "evidence" && parts.len() == 1)
                || (root == "durable-artifacts"
                    && (parts.len() == 1 || (parts.len() == 2 && operation))),
            "unsupported payload directory"
        );
    } else {
        let fixed = parts.len() == 1
            && matches!(
                root,
                "manifest.json"
                    | "database.dump"
                    | "config.toml"
                    | "scrape.toml"
                    | "inventory.json"
            );
        let evidence = root == "evidence"
            && parts.len() == 2
            && parts
                .get(1)
                .is_some_and(|name| name.strip_suffix(".body").is_some_and(digest));
        let durable = root == "durable-artifacts"
            && ((parts.len() == 2
                && parts.get(1).is_some_and(|name| {
                    name.rsplit_once('.').is_some_and(|(stem, extension)| {
                        digest(stem) && matches!(extension, "blob" | "capture" | "stop")
                    })
                }))
                || (parts.len() == 3
                    && parts.get(1).is_some_and(|name| {
                        name.strip_prefix("operation-").is_some_and(digest)
                            || (*name == "inputs"
                                && parts.get(2).is_some_and(|name| {
                                    name.strip_suffix(".input").is_some_and(digest)
                                }))
                    })));
        ensure!(
            fixed || evidence || durable || (root == "exports" && parts.len() > 1),
            "unsupported payload path"
        );
    }
    Ok(())
}

pub(super) fn portable(value: &str, seen: &mut BTreeMap<String, String>) -> Result<()> {
    value
        .split('/')
        .try_fold(String::new(), |prefix, part| {
            let prefix = if prefix.is_empty() {
                part.to_owned()
            } else {
                format!("{prefix}/{part}")
            };
            let folded = prefix.nfc().case_fold().collect::<String>();
            ensure!(
                seen.get(&folded).is_none_or(|old| old == &prefix),
                "archive paths collide on a case-insensitive filesystem"
            );
            seen.insert(folded, prefix.clone());
            Ok(prefix)
        })
        .map(|_| ())
}

pub(super) fn absolute(path: &Path) -> Result<PathBuf> {
    ensure!(
        !path
            .components()
            .any(|part| matches!(part, Component::ParentDir)),
        "paths must not contain '..'"
    );
    let absolute = std::path::absolute(path)?;
    absolute.ancestors().try_for_each(|part| {
        match fs::symlink_metadata(part) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "symbolic links are not allowed in selected paths"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    })?;
    Ok(absolute)
}

pub(super) fn source(path: &Path, directory: bool) -> Result<PathBuf> {
    let path = absolute(path)?;
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        if directory {
            metadata.is_dir()
        } else {
            metadata.file_type().is_file()
        },
        "selected source has an unsupported file type"
    );
    Ok(path)
}

pub(super) fn config(path: &Path) -> Result<()> {
    ensure!(
        fs::metadata(path)?.len() <= super::manifest::SMALL,
        "configuration exceeds1MiB"
    );
    let text = fs::read_to_string(path)?;
    let secrets = [
        "password",
        "passwd",
        "secret",
        "token",
        "credential",
        "authorization",
        "cookie",
        "api_key",
        "api-key",
        "database_url",
        "database-url",
        "private_key",
        "private-key",
    ];
    ensure!(
        !text
            .lines()
            .filter_map(|line| line.split_once('='))
            .any(|(key, _)| secrets
                .iter()
                .any(|secret| key.case_fold().collect::<String>().contains(secret))),
        "configuration contains credential settings"
    );
    let pattern = regex::Regex::new(
        r#"(?i)[a-z][a-z0-9+.-]*://[^\s/"']*@|-----BEGIN [A-Z ]*PRIVATE KEY-----|[?&](?:password|passwd|token|access_token|secret|api[_-]?key)="#,
    )?;
    ensure!(
        !pattern.is_match(&text),
        "configuration contains embedded credentials"
    );
    Ok(())
}
