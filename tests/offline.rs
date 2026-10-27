//! Offline local hooks, fixer mode, and best-effort `ahi migrate`.
//!
//! The cargo-backed case is Ian's rewritten config. A second case uses `git`
//! so the suite still runs where cargo is not on PATH.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

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

fn temp_repo(label: &str) -> PathBuf {
    let dir = fixture_directory::create_unique_fixture_directory(label);
    git(&dir, &["init", "-q", "--template="]);
    git(&dir, &["config", "user.email", "ahi@example.com"]);
    git(&dir, &["config", "user.name", "ahi"]);
    git(&dir, &["config", "core.autocrlf", "false"]);
    dir
}

fn example(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name);
    fs::read_to_string(path).unwrap()
}

#[test]
fn system_hook_without_a_policy_prints_the_snippet_and_does_not_run() {
    let dir = temp_repo("snippet");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: harmless\n    name: Harmless\n    entry: git status --porcelain\n    language: system\n    pass_filenames: false\n    always_run: true\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let output = run(&dir, &["run", "--all-files"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("trusted_system_hooks:"), "{text}");
    assert!(text.contains("id: harmless"), "{text}");
    assert!(text.contains("entry: \"git status --porcelain\""), "{text}");
    assert!(text.contains("language `system`"), "{text}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn harmless_local_hook_runs_without_cargo() {
    let dir = temp_repo("harmless");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: harmless\n    name: Harmless\n    entry: git rev-parse --is-inside-work-tree\n    language: system\n    pass_filenames: false\n    always_run: true\n    verbose: true\n",
    );
    write(
        &dir.join("ahi.policy.yaml"),
        b"version: 1\ntrusted_system_hooks:\n- id: harmless\n  entry: git rev-parse --is-inside-work-tree\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let cache = dir.join("cache");
    let output = run(
        &dir,
        &[
            "--cache-dir",
            cache.to_str().unwrap(),
            "run",
            "--all-files",
            "--verbose",
        ],
    );
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(text.contains("true"), "{text}");
    assert!(!text.contains("substituted"), "{text}");
    let audit = fs::read_to_string(cache.join("audit.log")).expect("audit log");
    assert!(audit.contains("harmless"), "{audit}");
    assert!(!audit.contains("substitut"), "{audit}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ahi_ids_stay_report_only_until_fix() {
    let dir = temp_repo("report");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: builtin\n  hooks:\n  - id: ahi/trailing-whitespace\n  - id: ahi/end-of-file\n  - id: ahi/line-endings\n",
    );
    write(&dir.join("notes.txt"), b"hello \nworld\r\n");
    git(&dir, &["add", "-A"]);
    let report = run(&dir, &["run", "--all-files"]);
    assert_eq!(report.status.code(), Some(1));
    assert_eq!(
        fs::read(dir.join("notes.txt")).unwrap(),
        b"hello \nworld\r\n"
    );
    let fixed = run(&dir, &["run", "--all-files", "--fix"]);
    assert_eq!(fixed.status.code(), Some(1));
    let bytes = fs::read(dir.join("notes.txt")).unwrap();
    assert_eq!(
        bytes,
        b"hello\nworld\n",
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn fixer_refuses_a_file_with_unstaged_changes() {
    let dir = temp_repo("dirty");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: builtin\n  hooks:\n  - id: ahi/end-of-file\n",
    );
    write(&dir.join("notes.txt"), b"hello\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    write(&dir.join("notes.txt"), b"hello");
    git(&dir, &["add", "notes.txt"]);
    write(&dir.join("notes.txt"), b"hello dirty");
    let output = run(&dir, &["run", "--fix"]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("unstaged"), "{text}");
    assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"hello dirty");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn wrong_entry_is_not_authorized_and_other_languages_stay_unsupported() {
    let dir = temp_repo("mismatch");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: harmless\n    name: Harmless\n    entry: git status --porcelain\n    language: system\n    pass_filenames: false\n    always_run: true\n  - id: local-py\n    name: Local Python\n    entry: python hook.py\n    language: python\n  - id: local-node\n    name: Local Node\n    entry: node hook.js\n    language: node\n",
    );
    write(
        &dir.join("ahi.policy.yaml"),
        b"version: 1\ntrusted_system_hooks:\n- id: harmless\n  entry: 'git status'\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let output = run(&dir, &["run", "--all-files"]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("does not match") || text.contains("entry: \"git status --porcelain\""),
        "{text}"
    );
    assert!(text.contains("not supported until v0.3"), "{text}");
    assert!(text.contains("language `node`"), "{text}");
    assert!(text.contains("not supported by ahi"), "{text}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn remote_without_substitution_is_not_fetched() {
    let dir = temp_repo("nofetch");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: https://github.com/pre-commit/pre-commit-hooks\n  rev: v6.0.0\n  hooks:\n  - id: check-yaml\n",
    );
    write(&dir.join("ok.yml"), b"a: 1\n");
    git(&dir, &["add", "-A"]);
    let output = run(&dir, &["run", "--all-files"]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("not executed until v0.3"), "{text}");
    assert!(!text.contains("substituted"), "{text}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn migrate_prints_a_suggestion_and_writes_only_with_force() {
    let dir = temp_repo("migrate");
    let original = "\
repos:
- repo: https://github.com/pre-commit/pre-commit-hooks
  rev: v6.0.0
  hooks:
  - id: check-yaml
  - id: end-of-file-fixer
  - id: trailing-whitespace
  - id: check-ast
- repo: local
  hooks:
  - id: rust-linting
    name: Rust linting
    entry: cargo fmt --all --
    language: system
    pass_filenames: true
    types: [file, rust]
  - id: rust-clippy
    name: Rust clippy
    entry: cargo clippy --all-targets --all-features -- -Dclippy::all
    language: system
    pass_filenames: false
    types: [file, rust]
";
    write(&dir.join(".pre-commit-config.yaml"), original.as_bytes());
    git(&dir, &["add", "-A"]);
    let help = run(&dir, &["migrate", "--help"]);
    let help_text = String::from_utf8(help.stdout).unwrap();
    assert!(
        help_text.contains("does not guarantee") || help_text.contains("feature parity"),
        "{help_text}"
    );
    let dry = run(&dir, &["migrate"]);
    let stdout = String::from_utf8(dry.stdout).unwrap();
    let stderr = String::from_utf8(dry.stderr).unwrap();
    assert_eq!(dry.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(stdout.contains("does not guarantee"), "{stdout}");
    assert!(stdout.contains("ahi/check-yaml"), "{stdout}");
    assert!(stdout.contains("ahi/end-of-file"), "{stdout}");
    assert!(stdout.contains("ahi/trailing-whitespace"), "{stdout}");
    assert!(stdout.contains("rust-linting"), "{stdout}");
    assert!(stdout.contains("entry: \"cargo fmt --all --\""), "{stdout}");
    assert!(stdout.contains("check-ast"), "{stdout}");
    assert!(stdout.contains("not fetched"), "{stdout}");
    assert!(stderr.contains("unchanged"), "{stderr}");
    let unchanged = fs::read_to_string(dir.join(".pre-commit-config.yaml")).unwrap();
    assert_eq!(unchanged, original);
    let refused = run(&dir, &["migrate", "--write"]);
    assert_eq!(refused.status.code(), Some(1));
    let refused_err = String::from_utf8(refused.stderr).unwrap();
    assert!(refused_err.contains("--force"), "{refused_err}");
    assert_eq!(
        fs::read_to_string(dir.join(".pre-commit-config.yaml")).unwrap(),
        original
    );
    let wrote = run(&dir, &["migrate", "--write", "--force"]);
    assert_eq!(
        wrote.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&wrote.stderr)
    );
    let updated = fs::read_to_string(dir.join(".pre-commit-config.yaml")).unwrap();
    assert!(updated.contains("repo: builtin"));
    assert!(updated.contains("repo: local"));
    assert!(updated.contains("rust-linting"));
    assert!(updated.contains("does not guarantee"));
    assert!(!updated.contains("https://github.com/pre-commit/pre-commit-hooks"));
    assert!(!updated.contains("check-ast"));
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn script_hook_receives_filenames_without_a_shell() {
    let dir = temp_repo("script");
    write(
        &dir.join("record.sh"),
        b"#!/bin/sh\nset -eu\nif [ \"${SECRET_TOKEN:-}\" != \"\" ]; then exit 3; fi\nif [ \"${PRE_COMMIT:-}\" != \"1\" ]; then exit 4; fi\nprintf '%s\\n' \"$@\" >> recorded.txt\n",
    );
    let mode = fs::metadata(dir.join("record.sh")).unwrap().permissions();
    let mut mode = mode;
    use std::os::unix::fs::PermissionsExt;
    mode.set_mode(0o755);
    fs::set_permissions(dir.join("record.sh"), mode).unwrap();
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: record\n    name: Record\n    entry: record.sh\n    language: script\n    pass_filenames: true\n    types: [text]\n    args: ['--flag']\n",
    );
    write(
        &dir.join("ahi.policy.yaml"),
        b"version: 1\ntrusted_system_hooks:\n- id: record\n  entry: record.sh\n",
    );
    write(&dir.join("a.txt"), b"a\n");
    write(&dir.join("b.txt"), b"b\n");
    git(&dir, &["add", "-A"]);
    let output = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .current_dir(&dir)
        .env("SECRET_TOKEN", "should-not-pass")
        .args(["run", "--all-files"])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recorded = fs::read_to_string(dir.join("recorded.txt")).unwrap();
    assert!(recorded.contains("--flag"), "{recorded}");
    assert!(recorded.contains("a.txt"), "{recorded}");
    assert!(recorded.contains("b.txt"), "{recorded}");
    assert!(!recorded.contains("should-not-pass"), "{recorded}");
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn toolchain_passthrough_is_names_only_and_drops_unlisted_vars() {
    let dir = temp_repo("env");
    write(
        &dir.join("envcheck.sh"),
        b"#!/bin/sh\nset -eu\n[ \"${PRE_COMMIT:-}\" = 1 ] || exit 4\n[ -n \"${CARGO_HOME:-}\" ] || exit 6\n[ -n \"${DEMO_TOOLCHAIN:-}\" ] || exit 7\n[ -z \"${UNLISTED_VAR:-}\" ] || exit 5\n[ -z \"${SECRET_TOKEN:-}\" ] || exit 3\n[ -z \"${GITHUB_TOKEN:-}\" ] || exit 8\nprintf 'ok\\n'\n",
    );
    let mut mode = fs::metadata(dir.join("envcheck.sh")).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    mode.set_mode(0o755);
    fs::set_permissions(dir.join("envcheck.sh"), mode).unwrap();
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: envcheck\n    name: Env\n    entry: envcheck.sh\n    language: script\n    pass_filenames: false\n    always_run: true\n    verbose: true\n",
    );
    write(
        &dir.join("ahi.policy.yaml"),
        b"version: 1\ntrusted_system_hooks:\n- id: envcheck\n  entry: envcheck.sh\n  toolchain_env: [DEMO_TOOLCHAIN]\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let cache = dir.join("cache");
    let output = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .current_dir(&dir)
        .env("CARGO_HOME", "/usr/local/cargo")
        .env("DEMO_TOOLCHAIN", "from-policy")
        .env("UNLISTED_VAR", "should-not-pass")
        .env("SECRET_TOKEN", "should-not-pass")
        .env("GITHUB_TOKEN", "should-not-pass")
        .args([
            "--cache-dir",
            cache.to_str().unwrap(),
            "run",
            "--all-files",
            "--verbose",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{text}\n{err}");
    assert!(text.contains("ok"), "{text}");
    let audit = fs::read_to_string(cache.join("audit.log")).unwrap();
    assert!(audit.contains("DEMO_TOOLCHAIN"), "{audit}");
    assert!(audit.contains("CARGO_HOME"), "{audit}");
    assert!(!audit.contains("from-policy"), "{audit}");
    assert!(!audit.contains("/usr/local/cargo"), "{audit}");
    assert!(!audit.contains("UNLISTED_VAR"), "{audit}");
    assert!(!audit.contains("SECRET_TOKEN"), "{audit}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn secret_toolchain_name_is_refused_before_the_hook_runs() {
    let dir = temp_repo("secret-env");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: envcheck\n    name: Env\n    entry: true\n    language: system\n    pass_filenames: false\n    always_run: true\n",
    );
    write(
        &dir.join("ahi.policy.yaml"),
        b"version: 1\ntoolchain_env: [AWS_PROFILE]\ntrusted_system_hooks:\n- id: envcheck\n  entry: \"true\"\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let output = run(&dir, &["run", "--all-files"]);
    let err = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("never passed through"), "{err}");
    assert!(err.contains("AWS_PROFILE"), "{err}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ians_config_runs_where_cargo_is_available() {
    if !cargo_works() {
        eprintln!(
            "skipping Ian's cargo config: cargo fmt and cargo clippy are not available under the scrubbed environment"
        );
        return;
    }
    let dir = temp_repo("ian");
    write(
        &dir.join(".pre-commit-config.yaml"),
        example("ian-repos/.pre-commit-config.yaml").as_bytes(),
    );
    write(
        &dir.join("ahi.policy.yaml"),
        example("ian-repos/ahi.policy.yaml").as_bytes(),
    );
    write(
        &dir.join("Cargo.toml"),
        b"[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    write(&dir.join("src/main.rs"), b"fn main() {}\n");
    write(&dir.join("notes.txt"), b"hello \nworld");
    git(&dir, &["add", "-A"]);
    let cache = dir.join("cache");
    let output = run(
        &dir,
        &[
            "--cache-dir",
            cache.to_str().unwrap(),
            "run",
            "--all-files",
            "--fix",
        ],
    );
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}\n{err}");
    assert!(!text.contains("substituted"), "{text}");
    assert!(hook_passed(&text, "Rust linting"), "{text}");
    assert!(hook_passed(&text, "Rust clippy"), "{text}");
    assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"hello\nworld\n");
    let audit = fs::read_to_string(cache.join("audit.log")).unwrap();
    assert!(audit.contains("rust-linting"), "{audit}");
    assert!(audit.contains("rust-clippy"), "{audit}");
    assert!(audit.contains("CARGO_HOME"), "{audit}");
    assert!(audit.contains("RUSTUP_HOME"), "{audit}");
    assert!(!audit.contains("substitut"), "{audit}");
    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        assert!(!audit.contains(&cargo_home), "{audit}");
    }
    let _ = fs::remove_dir_all(dir);
}

fn hook_passed(text: &str, name: &str) -> bool {
    text.lines()
        .any(|line| line.contains(name) && line.contains("Passed"))
}

/// Base names plus the default toolchain pass-through. Matches `local_hook`.
fn cargo_works() -> bool {
    const KEPT: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TERM",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
    ];
    let mut cmd = Command::new("cargo");
    cmd.args(["fmt", "--version"])
        .env_clear()
        .env("PRE_COMMIT", "1");
    for key in KEPT {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    let fmt = cmd.output();
    let mut clippy = Command::new("cargo");
    clippy
        .args(["clippy", "--version"])
        .env_clear()
        .env("PRE_COMMIT", "1");
    for key in KEPT {
        if let Ok(value) = std::env::var(key) {
            clippy.env(key, value);
        }
    }
    matches!(
        (fmt, clippy.output()),
        (Ok(fmt), Ok(clippy)) if fmt.status.success() && clippy.status.success()
    )
}
