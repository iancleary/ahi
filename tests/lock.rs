//! v0.2 lock, update, fetch, and verify against a local bare repository.
//!
//! The remote is a filesystem path, so the test does not contact GitHub.
//! `ahi run` after fetch is pointed at a git wrapper that records network
//! subcommands and fails if one is used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

fn ahi(dir: &Path, cache: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ahi"));
    command.current_dir(dir).arg("--cache-dir").arg(cache);
    command
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

struct Fixture {
    root: PathBuf,
    remote: PathBuf,
    consumer: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = fixture_directory::create_unique_fixture_directory(label);
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-q", "-b", "main", "--template="]);
        git(&source, &["config", "user.email", "ahi@example.com"]);
        git(&source, &["config", "user.name", "ahi"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        git(&source, &["config", "tag.gpgsign", "false"]);
        git(&source, &["config", "core.autocrlf", "false"]);
        write(&source.join("hook.txt"), b"one\n");
        git(&source, &["add", "hook.txt"]);
        git(&source, &["commit", "-qm", "one"]);
        git(&source, &["tag", "v1.0.0"]);
        write(&source.join("hook.txt"), b"two\n");
        git(&source, &["add", "hook.txt"]);
        git(&source, &["commit", "-qm", "two"]);
        git(&source, &["tag", "v1.1.0"]);
        git(&source, &["tag", "nightly"]);
        git(&source, &["branch", "feature"]);
        let remote = root.join("remote.git");
        git(
            &root,
            &["clone", "--bare", "--quiet", "source", "remote.git"],
        );
        let consumer = root.join("consumer");
        fs::create_dir(&consumer).unwrap();
        git(&consumer, &["init", "-q", "-b", "main", "--template="]);
        git(&consumer, &["config", "user.email", "ahi@example.com"]);
        git(&consumer, &["config", "user.name", "ahi"]);
        git(&consumer, &["config", "core.autocrlf", "false"]);
        Self {
            cache: root.join("cache"),
            remote,
            consumer,
            root,
        }
    }

    fn remote_url(&self) -> String {
        self.remote.display().to_string()
    }

    fn write_config(&self, rev: &str) {
        let repo = yaml_quote(&self.remote_url());
        let config = format!("repos:\n- repo: {repo}\n  rev: {rev}\n  hooks:\n  - id: sample\n");
        write(
            &self.consumer.join(".pre-commit-config.yaml"),
            config.as_bytes(),
        );
    }

    fn allow(&self, repo: &str) {
        let policy = format!("version: 1\nrepo_allow:\n- {}\n", yaml_quote(repo));
        write(&self.consumer.join("ahi.policy.yaml"), policy.as_bytes());
    }

    fn track(&self) {
        write(&self.consumer.join("ok.txt"), b"ok\n");
        git(&self.consumer, &["add", "-A"]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn lock_fetch_update_and_offline_run() {
    let fixture = Fixture::new("lock-flow");
    fixture.write_config("v1.0.0");
    fixture.allow(&fixture.remote_url());
    fixture.track();
    let dry_cache = fixture.root.join("dry-cache");
    let dry = ahi(&fixture.consumer, &dry_cache)
        .args(["update", "--dry-run"])
        .output()
        .unwrap();
    let dry_text = output_text(&dry);
    assert!(dry.status.success(), "{dry_text}");
    assert!(dry_text.contains("v1.1.0"), "{dry_text}");
    assert!(!dry_cache.exists(), "{dry_text}");
    assert!(!fixture.consumer.join("ahi.lock").exists());
    let config_before =
        fs::read_to_string(fixture.consumer.join(".pre-commit-config.yaml")).unwrap();
    assert!(config_before.contains("rev: v1.0.0"), "{config_before}");

    let locked = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    assert!(locked.status.success(), "{}", output_text(&locked));
    let lock_text = fs::read_to_string(fixture.consumer.join("ahi.lock")).unwrap();
    assert!(lock_text.contains("requested: \"v1.0.0\""), "{lock_text}");
    assert!(lock_text.contains("signature: \"absent\""), "{lock_text}");
    assert!(!lock_text.contains("v1.1.0"), "{lock_text}");
    let again = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    assert!(again.status.success(), "{}", output_text(&again));
    assert_eq!(
        fs::read_to_string(fixture.consumer.join("ahi.lock")).unwrap(),
        lock_text
    );

    let early = ahi(&fixture.consumer, &fixture.cache)
        .args(["run", "--all-files"])
        .output()
        .unwrap();
    let early_err = String::from_utf8(early.stderr).unwrap();
    assert_eq!(early.status.code(), Some(1), "{early_err}");
    assert!(early_err.contains("ahi fetch"), "{early_err}");

    let fetched = ahi(&fixture.consumer, &fixture.cache)
        .arg("fetch")
        .output()
        .unwrap();
    assert!(fetched.status.success(), "{}", output_text(&fetched));
    let verified = ahi(&fixture.consumer, &fixture.cache)
        .arg("verify")
        .output()
        .unwrap();
    let verified_text = output_text(&verified);
    assert!(verified.status.success(), "{verified_text}");
    assert!(
        verified_text.contains("signature absent"),
        "{verified_text}"
    );
    let required = ahi(&fixture.consumer, &fixture.cache)
        .args(["verify", "--require-signatures"])
        .output()
        .unwrap();
    assert_eq!(required.status.code(), Some(1));
    assert!(String::from_utf8(required.stderr)
        .unwrap()
        .contains("require-signatures"));

    run_without_network(&fixture);

    let digest_line = lock_text
        .lines()
        .find(|line| line.contains("digest:"))
        .unwrap()
        .to_string();
    let marker = "sha256:";
    let index = digest_line.find(marker).unwrap() + marker.len();
    let mut flipped = digest_line.clone();
    let replacement = if flipped.as_bytes()[index] == b'a' {
        "b"
    } else {
        "a"
    };
    flipped.replace_range(index..index + 1, replacement);
    let broken = lock_text.replace(&digest_line, &flipped);
    assert_ne!(broken, lock_text);
    fs::write(fixture.consumer.join("ahi.lock"), &broken).unwrap();
    let mismatch = ahi(&fixture.consumer, &fixture.cache)
        .arg("verify")
        .output()
        .unwrap();
    let mismatch_text = output_text(&mismatch);
    assert!(mismatch_text.contains("digest mismatch"), "{mismatch_text}");
    fs::write(fixture.consumer.join("ahi.lock"), &lock_text).unwrap();

    let preview = ahi(&fixture.consumer, &fixture.cache)
        .args(["autoupdate", "--dry-run"])
        .output()
        .unwrap();
    let preview_text = output_text(&preview);
    assert!(preview.status.success(), "{preview_text}");
    assert!(preview_text.contains("v1.0.0 -> v1.1.0"), "{preview_text}");
    assert_eq!(
        fs::read_to_string(fixture.consumer.join("ahi.lock")).unwrap(),
        lock_text
    );
    assert_eq!(
        fs::read_to_string(fixture.consumer.join(".pre-commit-config.yaml")).unwrap(),
        config_before
    );

    let updated = ahi(&fixture.consumer, &fixture.cache)
        .arg("update")
        .output()
        .unwrap();
    assert!(updated.status.success(), "{}", output_text(&updated));
    let config_after =
        fs::read_to_string(fixture.consumer.join(".pre-commit-config.yaml")).unwrap();
    assert!(config_after.contains("rev: v1.1.0"), "{config_after}");
    assert!(!config_after.contains("rev: v1.0.0"), "{config_after}");
    let lock_after = fs::read_to_string(fixture.consumer.join("ahi.lock")).unwrap();
    assert!(lock_after.contains("requested: \"v1.1.0\""), "{lock_after}");
    let repeat = ahi(&fixture.consumer, &fixture.cache)
        .arg("update")
        .output()
        .unwrap();
    assert!(repeat.status.success(), "{}", output_text(&repeat));
    assert_eq!(
        fs::read_to_string(fixture.consumer.join("ahi.lock")).unwrap(),
        lock_after
    );
    assert!(String::from_utf8(repeat.stdout)
        .unwrap()
        .contains("unchanged"));
}

#[cfg(unix)]
fn run_without_network(fixture: &Fixture) {
    let bin = fixture.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let real = real.trim();
    assert!(!real.is_empty());
    let log = fixture.root.join("git.log");
    let script = format!(
        "#!/bin/sh\nlog=\"$AHI_GIT_LOG\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    fetch|ls-remote|clone|pull|push)\n      printf '%s\\n' \"$*\" >> \"$log\"\n      exit 97\n      ;;\n  esac\ndone\nexec {real} \"$@\"\n"
    );
    let wrapper = bin.join("git");
    fs::write(&wrapper, script).unwrap();
    let mut mode = fs::metadata(&wrapper).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    mode.set_mode(0o755);
    fs::set_permissions(&wrapper, mode).unwrap();
    let mut command = ahi(&fixture.consumer, &fixture.cache);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut prefixed = std::ffi::OsString::from(bin);
    prefixed.push(":");
    prefixed.push(path);
    command
        .env("PATH", prefixed)
        .env("AHI_GIT_LOG", &log)
        .args(["run", "--all-files"]);
    let output = command.output().unwrap();
    let text = output_text(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("not executed until v0.3"), "{text}");
    let logged = fs::read_to_string(&log).unwrap_or_default();
    assert!(logged.is_empty(), "{logged}");
}

#[cfg(not(unix))]
fn run_without_network(fixture: &Fixture) {
    let output = ahi(&fixture.consumer, &fixture.cache)
        .args(["run", "--all-files"])
        .output()
        .unwrap();
    let text = output_text(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("not executed until v0.3"),
        "{text}"
    );
}

#[test]
fn branch_rev_and_non_version_tag_are_rejected() {
    let fixture = Fixture::new("lock-branch");
    fixture.allow(&fixture.remote_url());
    fixture.write_config("feature");
    fixture.track();
    let branch = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    let err = String::from_utf8(branch.stderr).unwrap();
    assert_eq!(branch.status.code(), Some(1), "{err}");
    assert!(err.contains("branch"), "{err}");
    assert!(!fixture.cache.join("mirrors").exists(), "{err}");

    fixture.write_config("nightly");
    let locked = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    assert!(locked.status.success(), "{}", output_text(&locked));
    let update = ahi(&fixture.consumer, &fixture.cache)
        .arg("update")
        .output()
        .unwrap();
    let err = String::from_utf8(update.stderr).unwrap();
    assert_eq!(update.status.code(), Some(1), "{err}");
    assert!(err.contains("not an immutable version tag"), "{err}");
}

#[test]
fn commit_pin_is_not_moved() {
    let fixture = Fixture::new("lock-sha");
    let sha = git_stdout(&fixture.root.join("source"), &["rev-parse", "v1.0.0"]);
    assert_eq!(sha.len(), 40);
    fixture.allow(&fixture.remote_url());
    fixture.write_config(&sha);
    fixture.track();
    let updated = ahi(&fixture.consumer, &fixture.cache)
        .arg("update")
        .output()
        .unwrap();
    assert!(updated.status.success(), "{}", output_text(&updated));
    let config = fs::read_to_string(fixture.consumer.join(".pre-commit-config.yaml")).unwrap();
    assert!(config.contains(&sha), "{config}");
    assert!(!config.contains("v1.1.0"), "{config}");
    let lock = fs::read_to_string(fixture.consumer.join("ahi.lock")).unwrap();
    assert!(lock.contains(&format!("requested: \"{sha}\"")), "{lock}");
    assert!(lock.contains(&format!("commit: \"{sha}\"")), "{lock}");
}

#[test]
fn additional_dependencies_and_unknown_repos_fail_before_fetch() {
    let fixture = Fixture::new("lock-deny");
    fixture.allow(&fixture.remote_url());
    let repo = yaml_quote(&fixture.remote_url());
    let config = format!(
        "repos:\n- repo: {repo}\n  rev: v1.0.0\n  hooks:\n  - id: sample\n    additional_dependencies: [\"ruff>=0.1\"]\n"
    );
    write(
        &fixture.consumer.join(".pre-commit-config.yaml"),
        config.as_bytes(),
    );
    fixture.track();
    let denied = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    let err = String::from_utf8(denied.stderr).unwrap();
    assert_eq!(denied.status.code(), Some(1), "{err}");
    assert!(err.contains("additional_dependencies"), "{err}");
    assert!(!fixture.cache.join("mirrors").exists(), "{err}");

    fixture.write_config("v1.0.0");
    fixture.allow(&format!("{}-other", fixture.remote_url()));
    let blocked = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    let err = String::from_utf8(blocked.stderr).unwrap();
    assert_eq!(blocked.status.code(), Some(1), "{err}");
    assert!(err.contains("not in repo_allow"), "{err}");
    assert!(!fixture.cache.join("mirrors").exists(), "{err}");

    fixture.write_config("v9.9.9");
    fixture.allow(&fixture.remote_url());
    let missing = ahi(&fixture.consumer, &fixture.cache)
        .arg("lock")
        .output()
        .unwrap();
    let err = String::from_utf8(missing.stderr).unwrap();
    assert_eq!(missing.status.code(), Some(1), "{err}");
    assert!(err.contains("not found"), "{err}");
}
