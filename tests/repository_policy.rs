use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

#[test]
fn repository_source_contains_no_python_or_github_actions() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut inspected = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        ensure!(
            depth <= 64,
            "repository policy traversal exceeded depth bound"
        );
        for entry in std::fs::read_dir(&directory)? {
            inspected += 1;
            ensure!(
                inspected <= 100_000,
                "repository policy traversal exceeded entry bound"
            );
            let path = entry?.path();
            let relative = path.strip_prefix(root)?;
            if ignored(relative) {
                continue;
            }
            ensure!(
                !matches!(
                    path.extension().and_then(std::ffi::OsStr::to_str),
                    Some("py" | "pyi" | "pyc" | "pyo" | "ipynb")
                ),
                "Python source or bytecode found: {}",
                relative.display()
            );
            ensure!(
                !relative.starts_with(".github/workflows")
                    && !relative.starts_with(".github/actions"),
                "GitHub Actions content found: {}",
                relative.display()
            );
            let kind = std::fs::symlink_metadata(&path)?.file_type();
            if kind.is_dir() {
                pending.push((path, depth + 1));
            }
        }
    }
    ensure!(
        inspected > 0,
        "repository policy did not inspect source files"
    );
    Ok(())
}

fn ignored(relative: &Path) -> bool {
    if relative.starts_with(".moon/cache") {
        return true;
    }
    let first: Option<PathBuf> = relative
        .components()
        .next()
        .map(|part| part.as_os_str().into());
    first
        .as_deref()
        .is_some_and(|name| matches!(name.to_str(), Some(".git" | "target" | ".beads")))
}
