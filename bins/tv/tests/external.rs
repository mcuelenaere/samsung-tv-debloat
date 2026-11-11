//! External-subcommand discovery and built-in precedence, using a benign
//! shell helper rather than a TV.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn copied_tv() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let tv = dir.path().join("tv");
    fs::copy(env!("CARGO_BIN_EXE_tv"), &tv).unwrap();
    (dir, tv)
}

fn helper(path: &std::path::Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn runs_a_helper_beside_the_executable() {
    let (dir, tv) = copied_tv();
    helper(
        &dir.path().join("tv-example"),
        "echo helper; printf 'arg=%s\\n' \"$@\"; echo err >&2; exit 7",
    );
    let output = Command::new(&tv)
        .args(["example", "one", "two words"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("helper"), "{stdout}");
    assert!(stdout.contains("arg=one"), "{stdout}");
    assert!(stdout.contains("arg=two words"), "{stdout}");
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "err");
}

#[test]
fn falls_back_to_path_when_opted_in() {
    let (dir, tv) = copied_tv();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    helper(&bin.join("tv-pathonly"), "echo from-path");
    let output = Command::new(&tv)
        .arg("pathonly")
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "from-path");

    let output = Command::new(&tv)
        .arg("pathonly")
        .env("PATH", &bin)
        .env("TV_NO_PATH_HELPERS", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown command"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_helper_is_an_ordinary_error() {
    let (dir, tv) = copied_tv();
    let output = Command::new(&tv)
        .arg("does-not-exist")
        .env("TV_NO_PATH_HELPERS", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown command"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(dir);
}

#[test]
fn builtins_win_over_helpers() {
    let (dir, tv) = copied_tv();
    helper(&dir.path().join("tv-sdb"), "echo helper-sdb");
    let output = Command::new(&tv).args(["sdb", "--help"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("usage: tv sdb"), "{stdout}");
    assert!(!stdout.contains("helper-sdb"));
}

#[test]
fn ssh_key_rejects_invalid_input_before_contacting_a_tv() {
    let (dir, tv) = copied_tv();
    let key = dir.path().join("bad.pub");
    fs::write(&key, "ssh-rsa not-a-real-key\n").unwrap();
    let output = Command::new(&tv)
        .args([
            OsString::from("ssh-key"),
            OsString::from("add"),
            OsString::from("192.0.2.10"),
            key.into_os_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a valid OpenSSH public key"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
