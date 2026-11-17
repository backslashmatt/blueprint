//! The CLI binary starts and reports its version.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::process::Command;

#[test]
fn version_flag_prints_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_bp"))
        .arg("--version")
        .output()
        .expect("run bp");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert_eq!(stdout.trim(), format!("bp {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_command_fails_with_usage() {
    let out = Command::new(env!("CARGO_BIN_EXE_bp"))
        .arg("no-such-command")
        .output()
        .expect("run bp");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    assert!(stderr.contains("Usage: bp"), "stderr was: {stderr}");
}
