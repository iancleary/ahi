//! CLI behavior that needs a git repository: golden `ahi run` output, and
//! the v0.3 refusal for hooks this slice does not execute.
//!
//! JSON and SARIF snapshots include the crate version. Comparison substitutes
//! `env!("CARGO_PKG_VERSION")` into that field. Text output is compared as
//! stored. The live command output must still report the crate version.

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

fn plain_json_string(value: &str) -> bool {
    !value.contains('"') && !value.contains('\\')
}

fn json_string_field_bounds(text: &str, key: &str) -> (usize, usize) {
    let needle = format!("\"{key}\": \"");
    let count = text.matches(&needle).count();
    assert_eq!(count, 1, "expected exactly one {key} string field");
    let key_at = text
        .find(&needle)
        .unwrap_or_else(|| panic!("missing {key}"));
    let value_at = key_at + needle.len();
    let end_rel = text[value_at..]
        .find('"')
        .unwrap_or_else(|| panic!("unterminated {key}"));
    let value_end = value_at + end_rel;
    assert!(
        plain_json_string(&text[value_at..value_end]),
        "{key} value contains an escape"
    );
    (value_at, value_end)
}

fn json_string_field(text: &str, key: &str) -> String {
    let (start, end) = json_string_field_bounds(text, key);
    text[start..end].to_string()
}

fn replace_json_string_value(text: &str, key: &str, new_value: &str) -> String {
    assert!(
        plain_json_string(new_value),
        "replacement must be a plain JSON string"
    );
    let (start, end) = json_string_field_bounds(text, key);
    let mut out = String::with_capacity(text.len() + new_value.len());
    out.push_str(&text[..start]);
    out.push_str(new_value);
    out.push_str(&text[end..]);
    out
}

const SARIF_DRIVER_VERSION_NEEDLE: &str = "\n          \"version\": \"";

fn sarif_driver_version_bounds(text: &str) -> (usize, usize) {
    let driver_count = text.matches(SARIF_DRIVER_VERSION_NEEDLE).count();
    assert_eq!(driver_count, 1, "expected exactly one SARIF driver version");
    let schema_count = text.matches("\n  \"version\": \"").count();
    assert_eq!(schema_count, 1, "expected the SARIF schema version field");
    let key_at = text
        .find(SARIF_DRIVER_VERSION_NEEDLE)
        .expect("driver version");
    let value_at = key_at + SARIF_DRIVER_VERSION_NEEDLE.len();
    let end_rel = text[value_at..]
        .find('"')
        .expect("unterminated driver version");
    let value_end = value_at + end_rel;
    let value = &text[value_at..value_end];
    assert_ne!(
        value, "2.1.0",
        "refusing to treat the SARIF schema version as the tool version"
    );
    assert!(
        plain_json_string(value),
        "driver version contains an escape"
    );
    (value_at, value_end)
}

fn sarif_driver_version(text: &str) -> String {
    let (start, end) = sarif_driver_version_bounds(text);
    text[start..end].to_string()
}

fn replace_sarif_driver_version(text: &str, new_value: &str) -> String {
    assert!(
        plain_json_string(new_value),
        "replacement must be a plain JSON string"
    );
    assert_ne!(
        new_value, "2.1.0",
        "replacement collides with the schema version"
    );
    let (start, end) = sarif_driver_version_bounds(text);
    let mut out = String::with_capacity(text.len() + new_value.len());
    out.push_str(&text[..start]);
    out.push_str(new_value);
    out.push_str(&text[end..]);
    assert_eq!(
        out.matches("\n  \"version\": \"").count(),
        1,
        "schema version field changed"
    );
    out
}

fn substitute_crate_version(name: &str, bytes: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).expect("golden output is utf-8");
    let version = env!("CARGO_PKG_VERSION");
    let substituted = match name {
        "run.json" => replace_json_string_value(text, "ahi_version", version),
        "run.sarif" => replace_sarif_driver_version(text, version),
        _ => return bytes.to_vec(),
    };
    substituted.into_bytes()
}

fn reported_crate_version(name: &str, bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).expect("golden output is utf-8");
    match name {
        "run.json" => Some(json_string_field(text, "ahi_version")),
        "run.sarif" => Some(sarif_driver_version(text)),
        _ => None,
    }
}

fn golden_difference(name: &str, actual: &[u8]) -> Option<String> {
    let path = golden(name);
    let expected = fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "missing {} ({err}); re-run with UPDATE_GOLDEN=1 after reviewing the output",
            path.display()
        )
    });
    let expected = substitute_crate_version(name, &expected);
    let actual = substitute_crate_version(name, actual);
    if expected == actual {
        None
    } else {
        Some(format!(
            "golden {name} drifted\n--- expected\n{}\n--- actual\n{}",
            String::from_utf8_lossy(&expected),
            String::from_utf8_lossy(&actual)
        ))
    }
}

fn expect_golden(name: &str, actual: &[u8]) {
    let path = golden(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    if let Some(reported) = reported_crate_version(name, actual) {
        assert_eq!(
            reported,
            env!("CARGO_PKG_VERSION"),
            "{name} reported {reported}"
        );
    }
    if let Some(diff) = golden_difference(name, actual) {
        panic!("{diff}");
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
fn crate_version_change_does_not_fail_golden_json_or_sarif() {
    let dir = fixture();
    let json = run(&dir, &["run", "--all-files", "--format", "json"]);
    assert_eq!(json.status.code(), Some(1));
    let json_text = String::from_utf8(json.stdout).unwrap();
    assert_eq!(
        json_string_field(&json_text, "ahi_version"),
        env!("CARGO_PKG_VERSION")
    );
    let rewritten_json = replace_json_string_value(&json_text, "ahi_version", "9.9.9");
    assert!(
        golden_difference("run.json", rewritten_json.as_bytes()).is_none(),
        "a crate version change must not fail the JSON golden comparison"
    );
    let broken_json = rewritten_json.replacen("trailing whitespace", "trailing spaces", 1);
    assert_ne!(broken_json, rewritten_json);
    assert!(
        golden_difference("run.json", broken_json.as_bytes()).is_some(),
        "a finding change must still fail the JSON golden comparison"
    );

    let sarif = run(&dir, &["run", "--all-files", "--format", "sarif"]);
    assert_eq!(sarif.status.code(), Some(1));
    let sarif_text = String::from_utf8(sarif.stdout).unwrap();
    assert_eq!(sarif_driver_version(&sarif_text), env!("CARGO_PKG_VERSION"));
    let rewritten_sarif = replace_sarif_driver_version(&sarif_text, "9.9.9");
    assert!(
        golden_difference("run.sarif", rewritten_sarif.as_bytes()).is_none(),
        "a crate version change must not fail the SARIF golden comparison"
    );
    let broken_sarif = rewritten_sarif.replacen("trailing whitespace", "trailing spaces", 1);
    assert_ne!(broken_sarif, rewritten_sarif);
    assert!(
        golden_difference("run.sarif", broken_sarif.as_bytes()).is_some(),
        "a finding change must still fail the SARIF golden comparison"
    );
    let broken_schema = sarif_text.replacen(
        "\n  \"version\": \"2.1.0\"",
        "\n  \"version\": \"9.9.9\"",
        1,
    );
    assert_ne!(broken_schema, sarif_text);
    assert!(
        golden_difference("run.sarif", broken_schema.as_bytes()).is_some(),
        "the SARIF schema version must still be compared"
    );
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
