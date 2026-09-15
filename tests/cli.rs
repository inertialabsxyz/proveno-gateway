//! Pins the command-line surface: every subcommand parses its arguments and
//! reports a setup failure.

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_proveno-gateway"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn check_requires_principal() {
    let out = run(&["check", "--config", "/nonexistent", "program.lua"]);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("not implemented"));
}

#[test]
fn replay_with_an_unreadable_config_fails() {
    let out = run(&["replay", "--config", "/nonexistent", "0190"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("replay failed"), "{stderr}");
    assert!(stderr.contains("/nonexistent"), "{stderr}");
}

#[test]
fn serve_and_check_report_a_missing_config() {
    let program = tempfile::NamedTempFile::new().unwrap();
    let program = program.path().to_str().unwrap();
    for args in [
        &["serve", "--config", "/nonexistent"][..],
        &[
            "check",
            "--config",
            "/nonexistent",
            "--principal",
            "demo-agent",
            program,
        ],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("/nonexistent"), "{args:?}: {stderr}");
    }
}
