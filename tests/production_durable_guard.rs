use std::process::{Command, Output};

use anyhow::Result;

fn run(directory: &std::path::Path, flag: Option<&str>, arguments: &[&str]) -> Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_track-scrapper-v2"));
    command.env_remove("SCRAPER_REQUIRE_DURABLE");
    if let Some(flag) = flag {
        command.env("SCRAPER_REQUIRE_DURABLE", flag);
    }
    Ok(command
        .arg("--database-url")
        .arg("not-a-database-url")
        .args(arguments)
        .current_dir(directory)
        .output()?)
}

#[test]
fn removed_direct_commands_are_unavailable_with_or_without_environment_flags() -> Result<()> {
    let root = tempfile::tempdir()?;
    let legacy = [
        vec!["scrape", "--config", "absent.toml"],
        vec!["export", "--config", "absent.toml"],
        vec!["scrape", "--config", "absent.toml", "--wiaa-directory"],
        vec![
            "scrape",
            "--config",
            "absent.toml",
            "--download-school-addresses",
            "absent.csv",
            "--school-addresses-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
        vec![
            "scrape",
            "--config",
            "absent.toml",
            "--wiaa-coop-manifest",
            "absent.json",
        ],
        vec![
            "scrape",
            "--config",
            "absent.toml",
            "--athlete-identity-manifest",
            "absent.json",
            "--athlete-identity-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
        vec![
            "scrape",
            "--input",
            "absent.html",
            "--source",
            "milesplit",
            "--kind",
            "athlete",
            "--url",
            "https://wi.milesplit.com/athletes/1",
        ],
    ];
    for flag in [None, Some("true"), Some("1"), Some("false"), Some("")] {
        for arguments in &legacy {
            let output = run(root.path(), flag, arguments)?;
            assert_eq!(output.status.code(), Some(2));
            let error = String::from_utf8(output.stderr)?;
            assert!(
                error.contains("unrecognized subcommand"),
                "{flag:?} {arguments:?}: {error}"
            );
            assert!(!error.contains("SCRAPER_REQUIRE_DURABLE"));
        }
    }
    assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    Ok(())
}

#[test]
fn durable_is_the_only_advertised_action_and_keeps_its_own_validation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let output = run(root.path(), None, &["--help"])?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    assert!(
        text.lines()
            .any(|line| line.split_whitespace().next() == Some("durable"))
    );
    assert!(
        !text
            .lines()
            .any(|line| matches!(line.split_whitespace().next(), Some("scrape" | "export")))
    );
    assert!(text.contains("--database-url"));
    for flag in [None, Some("true"), Some("invalid")] {
        let output = run(
            root.path(),
            flag,
            &["durable", "status", "--key", "../invalid"],
        )?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("identifier must contain"));
    }
    Ok(())
}
