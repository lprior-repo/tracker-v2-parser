use clap::Parser;
use track_scrapper_v2::native_ops::recovery::RecoveryArgs;

#[derive(Parser)]
struct RecoveryCommand {
    #[command(flatten)]
    args: RecoveryArgs,
}

#[test]
fn native_recovery_requires_all_owned_binary_and_evidence_paths() {
    assert!(
        RecoveryCommand::try_parse_from([
            "proof",
            "--server",
            "/native/server",
            "--worker",
            "/native/worker"
        ])
        .is_err()
    );
    let command = RecoveryCommand::try_parse_from([
        "proof",
        "--server",
        "/native/server",
        "--worker",
        "/native/worker",
        "--evidence",
        "/new/private/evidence",
    ])
    .expect("valid explicit proof arguments");
    assert_eq!(command.args.server, std::path::Path::new("/native/server"));
    assert_eq!(command.args.worker, std::path::Path::new("/native/worker"));
    assert_eq!(
        command.args.evidence,
        std::path::Path::new("/new/private/evidence")
    );
}
