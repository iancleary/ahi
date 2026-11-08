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
    assert!(text.contains("action: hook"), "{text}");
    assert!(
        text.contains("match: {id: harmless, entry: \"git status --porcelain\"}"),
        "{text}"
    );
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
        &dir.join("ahi.hooks.yaml"),
        b"version: 1\nrules:\n- {id: harmless, action: hook, match: {id: harmless, entry: git rev-parse --is-inside-work-tree}, effect: approve}\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let cache = dir.join("state");
    let output = run(
        &dir,
        &[
            "--state-dir",
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
        &dir.join("ahi.hooks.yaml"),
        b"version: 1\nrules:\n- {id: harmless, action: hook, match: {id: harmless, entry: 'git status'}, effect: approve}\n",
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
    assert!(text.contains("language `python`"), "{text}");
    assert!(text.contains("not supported by ahi"), "{text}");
    assert!(!text.contains("until v0.3"), "{text}");
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
    assert!(
        text.contains("from a remote repo is not executed"),
        "{text}"
    );
    assert!(!text.contains("substituted"), "{text}");
    assert!(!text.contains("until v0.3"), "{text}");
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
    assert!(stdout.contains("unchanged"), "{stdout}\n{stderr}");
    assert!(stdout.contains("Add to ahi.hooks.yaml"), "{stdout}");
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
        &dir.join("ahi.hooks.yaml"),
        b"version: 1\nrules:\n- {id: record, action: hook, match: {id: record, entry: record.sh}, effect: approve}\n",
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
        &dir.join("ahi.hooks.yaml"),
        b"version: 1\nrules:\n- {id: envcheck, action: hook, match: {id: envcheck, entry: envcheck.sh}, effect: approve, toolchain_env: [DEMO_TOOLCHAIN]}\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let cache = dir.join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .current_dir(&dir)
        .env("CARGO_HOME", "/usr/local/cargo")
        .env("DEMO_TOOLCHAIN", "from-policy")
        .env("UNLISTED_VAR", "should-not-pass")
        .env("SECRET_TOKEN", "should-not-pass")
        .env("GITHUB_TOKEN", "should-not-pass")
        .args([
            "--state-dir",
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
        b"repos:\n- repo: local\n  hooks:\n  - id: envcheck\n    name: Env\n    entry: 'true'\n    language: system\n    pass_filenames: false\n    always_run: true\n",
    );
    write(
        &dir.join("ahi.hooks.yaml"),
        b"version: 1\ntoolchain_env: [AWS_PROFILE]\nrules:\n- {id: envcheck, action: hook, match: {id: envcheck, entry: \"true\"}, effect: approve}\n",
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
        &dir.join("ahi.hooks.yaml"),
        example("ian-repos/ahi.hooks.yaml").as_bytes(),
    );
    write(
        &dir.join("Cargo.toml"),
        b"[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    write(&dir.join("src/main.rs"), b"fn main() {}\n");
    write(&dir.join("notes.txt"), b"hello \nworld");
    git(&dir, &["add", "-A"]);
    let cache = dir.join("state");
    let output = run(
        &dir,
        &[
            "--state-dir",
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

#[test]
fn the_retired_policy_filename_is_an_error_not_silence() {
    let dir = temp_repo("retired-policy");
    write(
        &dir.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: local\n  hooks:\n  - id: harmless\n    name: Harmless\n    entry: git status --porcelain\n    language: system\n    pass_filenames: false\n    always_run: true\n",
    );
    write(
        &dir.join("ahi.policy.yaml"),
        b"version: 1\ntrusted_system_hooks:\n- id: harmless\n  entry: git status --porcelain\n",
    );
    write(&dir.join("ok.txt"), b"ok\n");
    git(&dir, &["add", "-A"]);
    let output = run(&dir, &["run", "--all-files"]);
    assert_ne!(output.status.code(), Some(0));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("ahi.policy.yaml"), "{text}");
    assert!(text.contains("ahi.hooks.yaml"), "{text}");
    assert!(text.contains("no longer read"), "{text}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn tools_init_prints_a_policy_and_writes_only_with_write() {
    let dir = fixture_directory::create_unique_fixture_directory("tools-init");
    let policy = dir.join("ahi.tools.yaml");
    let absent = run(&dir, &["tools", "list"]);
    let absent_out = String::from_utf8(absent.stdout).unwrap();
    assert_eq!(absent.status.code(), Some(0));
    assert!(
        absent_out.starts_with("tools policy: absent"),
        "{absent_out}"
    );
    assert!(absent_out.contains("ahi tools init"), "{absent_out}");

    let all = run(&dir, &["tools", "examples"]);
    let listed = String::from_utf8(all.stdout).unwrap();
    assert_eq!(all.status.code(), Some(0));
    assert!(listed.starts_with("read-all"), "{listed}");
    assert!(listed.contains("(default)"), "{listed}");
    assert!(
        listed.contains("== deny-all (examples/tools/deny-all.yaml)"),
        "{listed}"
    );
    assert!(listed.contains(&example("tools/commands.yaml")), "{listed}");
    assert!(!listed.contains("invalid-version"), "{listed}");

    let one = run(&dir, &["tools", "examples", "list-only"]);
    let one_out = String::from_utf8(one.stdout).unwrap();
    assert!(one_out.starts_with("== list-only"), "{one_out}");
    assert!(
        one_out.contains(&example("tools/list-only.yaml")),
        "{one_out}"
    );
    assert!(!one_out.contains("== read-all"), "{one_out}");
    let missing = run(&dir, &["tools", "examples", "nope"]);
    assert_eq!(missing.status.code(), Some(2));

    let dry = run(&dir, &["tools", "init"]);
    let stdout = String::from_utf8(dry.stdout).unwrap();
    assert_eq!(dry.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("id: read-tree"), "{stdout}");
    assert!(stdout.contains("Left ahi.tools.yaml unchanged"), "{stdout}");
    assert!(!policy.exists());

    let unknown = run(&dir, &["tools", "init", "--example", "nope"]);
    assert_eq!(unknown.status.code(), Some(2));
    let err = String::from_utf8(unknown.stderr).unwrap();
    assert!(err.contains("ahi tools examples"), "{err}");
    assert!(!policy.exists());

    let wrote = run(&dir, &["tools", "init", "--write"]);
    assert_eq!(wrote.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&policy).unwrap(),
        example("tools/read-all.yaml")
    );

    let refused = run(&dir, &["tools", "init", "--example", "deny-all", "--write"]);
    assert_eq!(refused.status.code(), Some(1));
    let err = String::from_utf8(refused.stderr).unwrap();
    assert!(err.contains("--write --force"), "{err}");
    assert_eq!(
        fs::read_to_string(&policy).unwrap(),
        example("tools/read-all.yaml")
    );

    let replaced = run(
        &dir,
        &[
            "tools",
            "init",
            "--example",
            "deny-all",
            "--write",
            "--force",
        ],
    );
    assert_eq!(replaced.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&policy).unwrap(),
        example("tools/deny-all.yaml")
    );

    let doctor = run(&dir, &["doctor"]);
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert!(
        report.contains("tool rules: 0 approve, 0 ask, 2 deny"),
        "{report}"
    );
    assert!(report.contains("tools advertised: none"), "{report}");
    fs::remove_file(&policy).unwrap();
    let doctor = run(&dir, &["doctor"]);
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert!(report.contains("tools policy: absent"), "{report}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tools_init_colors_only_when_asked_on_a_pipe() {
    let dir = fixture_directory::create_unique_fixture_directory("tools-init-color");
    let plain = run(&dir, &["tools", "init"]);
    let plain_out = String::from_utf8(plain.stdout).unwrap();
    assert!(!plain_out.contains("\x1b["), "{plain_out}");
    let colored = run(&dir, &["--color", "always", "tools", "init", "--write"]);
    let colored_out = String::from_utf8(colored.stdout).unwrap();
    assert!(colored_out.contains("\x1b[48;2;"), "{colored_out}");
    assert!(colored_out.contains("approve"), "{colored_out}");
    // The file on disk is never colored.
    assert_eq!(
        fs::read_to_string(dir.join("ahi.tools.yaml")).unwrap(),
        example("tools/read-all.yaml")
    );
    let never = run(&dir, &["--color", "never", "tools", "list"]);
    assert!(!String::from_utf8(never.stdout).unwrap().contains("\x1b["));
    let shown = run(&dir, &["--color", "always", "tools", "list"]);
    assert!(String::from_utf8(shown.stdout)
        .unwrap()
        .contains("\x1b[48;2;"));
    let examples = run(
        &dir,
        &["--color", "always", "tools", "examples", "deny-all"],
    );
    assert!(String::from_utf8(examples.stdout)
        .unwrap()
        .contains("\x1b[48;2;90;35;35m"));
    let _ = fs::remove_dir_all(&dir);
}
