//! CLI behavior that needs a git repository: golden `ahi run` output, and
//! the v0.3 refusal for hooks this slice does not execute.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ahi() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ahi"))
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn run(dir: &Path, args: &[&str]) -> Output {
    ahi().current_dir(dir).args(args).output().expect("ahi")
}

const CONFIG: &str = "\
repos:
- repo: builtin
  hooks:
  - id: ahi/trailing-whitespace
  - id: ahi/end-of-file
  - id: ahi/merge-conflict
  - id: ahi/large-files
  - id: ahi/case-conflict
  - id: ahi/symlinks
  - id: ahi/check-json
  - id: ahi/check-yaml
  - id: ahi/check-toml
  - id: ahi/trojan-source
  - id: ahi/windows-reserved-names
  - id: ahi/private-key
  - id: ahi/github-actions
  - id: ahi/dockerfile
  - id: ahi/shell-strict
  - id: ahi/committed-dotenv
";

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ahi-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "ahi@example.com"]);
    git(&dir, &["config", "user.name", "ahi"]);
    git(&dir, &["config", "core.autocrlf", "false"]);
    write(&dir.join(".pre-commit-config.yaml"), CONFIG.as_bytes());
    // Trailing space and no final newline: two checks, one file.
    write(&dir.join("notes.txt"), b"hello \nworld");
    write(
        &dir.join("conflict.txt"),
        b"<<<<<<< HEAD\na\n=======\nb\n>>>>>>> other\n",
    );
    write(&dir.join("bad.json"), b"{\n");
    write(&dir.join("bad.yml"), b"a: [\n");
    write(&dir.join("bad.toml"), b"a =\n");
    write(&dir.join("script.sh"), b"#!/bin/sh\necho hi\n");
    write(
        &dir.join("Dockerfile"),
        b"FROM alpine:3.19\nRUN curl https://example.com/x | sh\n",
    );
    write(
        &dir.join(".github/workflows/ci.yml"),
        b"name: ci\n\"on\": [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    write(&dir.join(".env"), b"TOKEN=1\n");
    write(&dir.join(".env.example"), b"TOKEN=\n");
    write(
        &dir.join("key.pem"),
        b"-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\n",
    );
    write(&dir.join("bidi.txt"), "latin \u{202E}hidden\n".as_bytes());
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    dir
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn expect_golden(name: &str, actual: &[u8]) {
    let path = golden(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "missing {} ({err}); re-run with UPDATE_GOLDEN=1 after reviewing the output",
            path.display()
        )
    });
    if expected != actual {
        let actual_text = String::from_utf8_lossy(actual);
        let expected_text = String::from_utf8_lossy(&expected);
        panic!("golden {name} drifted\n--- expected\n{expected_text}\n--- actual\n{actual_text}");
    }
}

#[test]
fn version_matches_package_exactly() {
    let output = ahi().arg("--version").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text, format!("ahi {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn list_checks_and_explain() {
    let listed = ahi().args(["list-checks"]).output().unwrap();
    assert!(listed.status.success());
    let text = String::from_utf8(listed.stdout).unwrap();
    assert!(text.contains("ahi/trailing-whitespace"));
    assert!(text.contains("ahi/dockerfile"));

    let explained = ahi()
        .args(["explain", "ahi/shell-strict"])
        .output()
        .unwrap();
    assert!(explained.status.success());
    let text = String::from_utf8(explained.stdout).unwrap();
    assert!(text.contains("set -euo pipefail"));
    assert!(text.contains("version: ahi "));
    assert!(text.contains("CARGO_HOME"));
    assert!(text.contains("RUSTUP_HOME"));
    assert!(text.contains("RUSTUP_TOOLCHAIN"));
    assert!(text.contains("names only"));

    let missing = ahi().args(["explain", "nope"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn later_commands_fail_closed() {
    for cmd in [
        "install",
        "uninstall",
        "lock",
        "verify",
        "fetch",
        "autoupdate",
    ] {
        let output = ahi().arg(cmd).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{cmd}");
        let err = String::from_utf8(output.stderr).unwrap();
        assert!(err.contains("not implemented until"), "{cmd}: {err}");
    }
}

#[test]
fn rejects_unknown_color_and_format() {
    let color = ahi()
        .args(["--color", "blue", "list-checks"])
        .output()
        .unwrap();
    assert_eq!(color.status.code(), Some(2));
    let dir = fixture();
    let format = run(&dir, &["run", "--all-files", "--format", "xml"]);
    assert_eq!(format.status.code(), Some(2));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn run_all_files_matches_golden_output() {
    let dir = fixture();
    let text = run(&dir, &["run", "--all-files"]);
    assert_eq!(text.status.code(), Some(1));
    let stdout = String::from_utf8(text.stdout.clone()).unwrap();
    for needle in [
        "notes.txt:1:",
        "notes.txt:",
        "conflict.txt",
        "bad.json",
        "bad.yml",
        "bad.toml",
        "bidi.txt",
        "key.pem",
        "private key found",
        ".github/workflows/ci.yml",
        "Dockerfile",
        "script.sh",
        ".env:",
        "pipes curl",
        "digest",
        "final stage USER",
    ] {
        assert!(stdout.contains(needle), "missing {needle}\n{stdout}");
    }
    assert!(!stdout.contains(".env.example"));
    expect_golden("run.txt", &text.stdout);

    let json = run(&dir, &["run", "--all-files", "--format", "json"]);
    assert_eq!(json.status.code(), Some(1));
    expect_golden("run.json", &json.stdout);

    let sarif = run(&dir, &["run", "--all-files", "--format", "sarif"]);
    assert_eq!(sarif.status.code(), Some(1));
    expect_golden("run.sarif", &sarif.stdout);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn files_flag_limits_the_set() {
    let dir = fixture();
    let output = run(&dir, &["run", "--files", "ok.txt"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Passed"));
    assert!(!text.contains("Failed"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn remote_and_language_hooks_are_not_skipped() {
    let dir = fixture();
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: local-py\n    name: Local Python\n    entry: python hook.py\n    language: python\n  - id: local-node\n    name: Local Node\n    entry: node hook.js\n    language: node\n  - id: local-sys\n    name: Local System\n    entry: true\n    language: system\n- repo: https://example.invalid/hooks\n  rev: abc\n  hooks:\n  - id: remote-hook\n- repo: meta\n  hooks:\n  - id: identity\n",
    );
    let output = run(&dir, &["run", "--all-files"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("not supported until v0.3"), "{text}");
    assert!(text.contains("language `python`"));
    assert!(text.contains("not supported by ahi"));
    assert!(text.contains("language `node`"));
    assert!(text.contains("trusted_system_hooks"));
    assert!(text.contains("repo: meta"));
    assert!(
        text.contains("remote repo is not executed until v0.3")
            || text.contains("from a remote repo is not executed until v0.3")
    );
    assert!(!text.contains("Skipped"));
    let _ = fs::remove_dir_all(dir);
}
