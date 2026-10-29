use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use calamine::{Reader, Xlsx};
use serde_json::{Value, json};

use super::{io, review, runtime::Runtime};

#[cfg(test)]
mod tests;

fn rows(book: &mut Xlsx<std::io::BufReader<File>>, name: &str) -> Result<Vec<Vec<String>>> {
    let range = book.worksheet_range(name)?;
    ensure!(
        range.height() <= 10 && range.width() <= 1000,
        "owned fixture workbook dimensions exceed bound"
    );
    Ok(range
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect())
}

fn workbook(path: &Path, expected: &[Vec<String>]) -> Result<Value> {
    ensure!(
        fs::metadata(path)?.len() <= 64 * 1024 * 1024,
        "owned fixture workbook exceeds64MiB"
    );
    let mut book: Xlsx<_> = calamine::open_workbook(path)?;
    ensure!(
        book.sheet_names() == ["Athletes", "Review"],
        "workbook must contain exactly Athletes then Review"
    );
    let athlete = rows(&mut book, "Athletes")?;
    ensure!(
        athlete.len() == 2,
        "workbook must contain exactly one confirmed-cohort athlete"
    );
    let header = athlete.first().context("athlete header missing")?;
    let values = athlete.get(1).context("athlete row missing")?;
    let cell = |name: &str| -> &str {
        header
            .iter()
            .position(|title| title == name)
            .and_then(|index| values.get(index))
            .map_or("", String::as_str)
    };
    ensure!(
        cell("Name") == "Brice Fuller" && cell("Graduation year") == "2028",
        "explicit Brice Fuller cohort identity lost"
    );
    ensure!(
        values.iter().all(|value| !value.contains("Kingston Penn")),
        "unknown-cohort athlete was included"
    );
    let marks = [
        ("Outdoor 1600m PR [all levels; milesplit]", "4:12.69"),
        ("Outdoor 1600m PR [ms; milesplit]", "4:33.74"),
        ("Indoor 3000m PR [hs; milesplit]", "8:26.83"),
        ("XC 5K PR [hs; milesplit]", "14:49.06"),
    ];
    marks.iter().try_for_each(|(column, mark)| -> Result<()> {
        ensure!(
            cell(column).starts_with(mark),
            "published PR lost: {column}"
        );
        Ok(())
    })?;
    let review = review::verify(&rows(&mut book, "Review")?, expected)?;
    Ok(
        json!({"sheets":["Athletes","Review"],"rows_including_header":2,"review":review,"athlete":"Brice Fuller","published_graduation_year":2028,"unknown_cohort_excluded":"Kingston Penn","verified_pr_cells":marks.into_iter().map(|(key,value)|(key.to_owned(),json!(value))).collect::<serde_json::Map<_,_>>()}),
    )
}

fn descriptor<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value> {
    value
        .pointer(pointer)
        .with_context(|| format!("missing export descriptor{pointer}"))
}

pub(super) fn verify(runtime: &Runtime, result: &Value, expected: &[Vec<String>]) -> Result<Value> {
    let receipt = io::artifact(descriptor(result, "/receipt")?, &runtime.root)?;
    let body: Value = serde_json::from_slice(&io::read(&receipt, io::JSON_LIMIT)?)?;
    let publication = descriptor(&body, "/publication")?;
    let book_ref = descriptor(publication, "/workbook")?;
    let path = io::artifact(book_ref, &runtime.root)?;
    let manifest_ref = descriptor(publication, "/manifest")?;
    let manifest_path = io::artifact(manifest_ref, &runtime.root)?;
    let manifest: Value = serde_json::from_slice(&io::read(&manifest_path, io::JSON_LIMIT)?)?;
    ensure!(
        manifest.get("rows").and_then(Value::as_u64) == Some(1)
            && publication.get("promoted") == Some(&Value::Bool(true)),
        "publication did not promote exactly one fixture athlete"
    );
    let pointer = PathBuf::from(io::text(publication, "/canonical_pointer")?);
    ensure!(
        pointer.canonicalize()? == runtime.root.canonicalize()?.join("current-export.json"),
        "canonical pointer escaped fixed root"
    );
    let pointer_bytes = io::read(&pointer, io::JSON_LIMIT)?;
    ensure!(
        &serde_json::from_slice::<Value>(&pointer_bytes)? == publication,
        "canonical pointer differs from verified publication"
    );
    ensure!(
        Path::new(io::text(result, "/output")?) == path,
        "output differs from verified immutable workbook"
    );
    let delivery = runtime.args.evidence.join("delivered.xlsx");
    [
        (delivery.clone(), path.clone(), book_ref),
        (
            delivery.with_extension("manifest.json"),
            manifest_path,
            manifest_ref,
        ),
    ]
    .into_iter()
    .try_for_each(|(copy, source, reference)| -> Result<()> {
        let metadata = fs::symlink_metadata(&copy)?;
        let original = fs::metadata(&source)?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && (metadata.dev(), metadata.ino()) != (original.dev(), original.ino()),
            "delivery must be a distinct regular-file copy"
        );
        let (bytes, hash) = io::file_digest(&copy, crate::export::MAX_EXPORT_BYTES)?;
        ensure!(
            io::text(reference, "/sha256")? == hash
                && reference.get("bytes").and_then(Value::as_u64) == Some(bytes),
            "delivery hash differs from immutable artifact"
        );
        Ok(())
    })?;
    Ok(
        json!({"workbook":workbook(&path,expected)?,"delivery":delivery,"delivery_kind":"regular_copies","sha256":io::text(book_ref,"/sha256")?,"canonical_pointer_sha256":io::digest(&pointer_bytes),"frozen_manifest_sha256":io::text(&manifest,"/frozen_manifest/sha256")?,"publication":publication}),
    )
}

pub(super) fn isolate(
    runtime: &Runtime,
    verified: &Value,
    expected: &[Vec<String>],
) -> Result<Value> {
    let publication = descriptor(verified, "/publication")?;
    let delivery = runtime.args.evidence.join("delivered.xlsx");
    let edits=[(delivery.clone(),descriptor(publication,"/workbook")?),(delivery.with_extension("manifest.json"),descriptor(publication,"/manifest")?)].into_iter().map(|(destination,reference)|->Result<Value> {
        let source=io::artifact(reference,&runtime.root)?;
        let metadata=fs::symlink_metadata(&destination)?;let original=fs::metadata(&source)?;
        ensure!(!metadata.file_type().is_symlink() && (metadata.dev(),metadata.ino())!=(original.dev(),original.ino()),"test delivery aliases immutable source");
        let mut file=OpenOptions::new().append(true).open(&destination)?;
        file.write_all(b"\nowned-test-user-edit-isolation-proof\n")?;file.sync_all()?;
        let hash=io::file_digest(&destination,crate::export::MAX_EXPORT_BYTES)?.1;
        ensure!(hash!=io::text(reference,"/sha256")?,"delivery edit did not change user copy");
        io::artifact(reference,&runtime.root)?;
        Ok(json!({"path":destination,"edited_sha256":hash,"immutable_sha256":io::text(reference,"/sha256")?,"immutable_artifact_unchanged":true}))
    }).collect::<Result<Vec<_>>>()?;
    let pointer = Path::new(io::text(publication, "/canonical_pointer")?);
    ensure!(
        io::file_digest(pointer, io::JSON_LIMIT)?.1
            == io::text(verified, "/canonical_pointer_sha256")?,
        "user edit changed canonical pointer"
    );
    Ok(
        json!({"delivery_kind":"regular_copies","edited_copies":edits,"canonical_pointer_unchanged":true,"immutable_workbook_reopened":workbook(&io::artifact(descriptor(publication,"/workbook")?,&runtime.root)?,expected)?}),
    )
}
