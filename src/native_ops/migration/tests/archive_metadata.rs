use super::*;

fn record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let length = (0..4).fold(body.len() + 1, |length, _| {
        body.len() + length.to_string().len()
    });
    format!("{length}{body}")
}

#[test]
fn given_long_portable_payload_when_archived_then_bounded_pax_paths_roundtrip() -> Result<()> {
    let root = temporary()?;
    bundle(root.path())?;
    let name = format!(
        "exports/{}/{}/{}.xlsx",
        "a".repeat(100),
        "b".repeat(100),
        "c".repeat(100)
    );
    let output = root.path().join(&name);
    files::directory(output.parent().expect("longpath parent"))?;
    files::create(&output)?.write_all(b"workbook fixture")?;
    let mut manifest: Manifest = files::load(&root.path().join("manifest.json"), 64 * 1024 * 1024)?;
    std::fs::remove_file(root.path().join("manifest.json"))?;
    manifest.files = files::list(root.path())?
        .iter()
        .map(|path| files::fingerprint(root.path(), path))
        .collect::<Result<Vec<_>>>()?;
    files::json(&root.path().join("manifest.json"), &manifest)?;
    let work = temporary()?;
    let archive = work.path().join("long.tar.gz");
    archive::write(root.path(), &archive)?;
    let restored = temporary()?;
    archive::unpack(&archive, restored.path())?;
    assert_eq!(
        std::fs::read(restored.path().join(name))?,
        b"workbook fixture"
    );
    Ok(())
}

#[test]
fn given_untrusted_pax_fields_when_restoring_then_size_duplicate_and_unsupported_keys_are_rejected()
-> Result<()> {
    let work = temporary()?;
    for (index, body) in [
        record("GNU.sparse.name", "exports/file"),
        format!(
            "{}{}",
            record("path", "exports/a"),
            record("path", "exports/b")
        ),
        "999 path=exports/a\n".into(),
    ]
    .iter()
    .enumerate()
    {
        let archive = work.path().join(format!("pax-{index}.tar.gz"));
        crafted(
            &archive,
            "pax-header",
            b'x',
            body.as_bytes(),
            u64::try_from(body.len())?,
        )?;
        let target = temporary()?;
        let error = archive::unpack(&archive, target.path()).expect_err("invalid metadata");
        assert!(format!("{error:#}").contains("PAX"));
        assert!(files::list(target.path())?.is_empty());
    }
    Ok(())
}

#[test]
fn given_duplicate_members_or_extra_compressed_data_when_restoring_then_archive_is_rejected()
-> Result<()> {
    let work = temporary()?;
    let duplicate = work.path().join("duplicate.tar.gz");
    let writer =
        flate2::GzBuilder::new().write(files::create(&duplicate)?, flate2::Compression::default());
    let mut tar = tar::Builder::new(writer);
    let mut header = tar::Header::new_ustar();
    header.set_size(0);
    header.set_mode(0o600);
    header.set_path("config.toml")?;
    header.set_cksum();
    tar.append(&header, std::io::empty())?;
    tar.append(&header, std::io::empty())?;
    tar.into_inner()?.finish()?.sync_all()?;
    let target = temporary()?;
    assert!(
        format!(
            "{:#}",
            archive::unpack(&duplicate, target.path()).expect_err("duplicate member")
        )
        .contains("duplicate")
    );
    let root = temporary()?;
    bundle(root.path())?;
    let trailing = work.path().join("trailing.tar.gz");
    archive::write(root.path(), &trailing)?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&trailing)?
        .write_all(b"unlisted trailing data")?;
    let target = temporary()?;
    assert!(
        format!(
            "{:#}",
            archive::unpack(&trailing, target.path()).expect_err("trailing data")
        )
        .contains("trailing")
    );
    Ok(())
}
