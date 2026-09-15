//! Pins the command-line surface: every subcommand parses its arguments.

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_proveno-gateway"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn subcommands_parse_and_report_not_implemented() {
    for args in [
        &["serve", "--config", "/nonexistent"][..],
        &[
            "check",
            "--config",
            "/nonexistent",
            "--principal",
            "demo-agent",
            "program.lua",
        ],
        &["replay", "--config", "/nonexistent", "0190"],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not implemented"),
            "{args:?}"
        );
    }
}

#[test]
fn check_requires_principal() {
    let out = run(&["check", "--config", "/nonexistent", "program.lua"]);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("not implemented"));
}
