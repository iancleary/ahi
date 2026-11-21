//! `ahi delegate` CLI contracts: run without Rex on PATH, without a server,
//! and without contacting a gateway.
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

fn run(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ahi"))
        .current_dir(state)
        .env_remove("XDG_RUNTIME_DIR")
        .env("PATH", "")
        .env_remove("REX_SESSION")
        .env_remove("REX_BLOCK")
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}

fn prepared(purpose: &str) -> std::path::PathBuf {
    let state = fixture_directory::create_unique_fixture_directory(purpose);
    std::fs::write(
        state.join("models.json"),
        json!({"providers": {"local": {
            "baseUrl": "http://127.0.0.1:1/v1", "api": "openai-completions", "apiKey": "gateway",
            "models": [{"id": "vendor/model"}]
        }}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(state.join("prompt.txt"), "sentinel quartz-lantern\n").unwrap();
    state
}

#[test]
fn help_is_operator_facing_and_the_worker_entry_point_is_hidden() {
    let state = fixture_directory::create_unique_fixture_directory("delegate-help");
    let help = run(&state, &["delegate", "--help"]);
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("launch") && text.contains("status") && text.contains("cancel"));
    let top = run(&state, &["--help"]);
    let text = String::from_utf8_lossy(&top.stdout);
    assert!(text.contains("delegate"));
    assert!(!text.contains("internal-worker"), "{text}");
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn dry_run_binds_the_route_and_reserves_nothing() {
    let state = prepared("delegate-dry-run");
    let prompt = state.join("prompt.txt");
    let preview = run(
        &state,
        &[
            "delegate",
            "launch",
            "--model",
            "local/vendor/model",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--server",
            "127.0.0.1:4400",
            "--dry-run",
        ],
    );
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let value: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["reserved"], false);
    assert_eq!(value["gateway"], "local");
    assert_eq!(value["model"], "vendor/model");
    assert_eq!(value["placement"], "scratch");
    assert_eq!(value["authority"]["mode"], "plan");
    assert!(value["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "read_file"));
    assert!(!value["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "write_file"));
    assert_eq!(value["target"]["backend"], "rex_server");
    assert!(!String::from_utf8_lossy(&preview.stdout).contains("quartz-lantern"));
    assert!(
        !state.join("delegation").exists(),
        "a preview creates no directory"
    );

    // Unknown model: refused before any reservation, exit 3.
    let refused = run(
        &state,
        &[
            "delegate",
            "launch",
            "--model",
            "local/other",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--server",
            "127.0.0.1:4400",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(refused.stdout.is_empty());
    assert!(!state.join("delegation").exists());

    // A Rex label is not a session; refused as input, exit 2.
    let refused = run(
        &state,
        &[
            "delegate",
            "launch",
            "--model",
            "local/vendor/model",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--server",
            "127.0.0.1:4400",
            "--host-session",
            "main",
            "--dry-run",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!state.join("delegation").exists());
    std::fs::remove_dir_all(state).unwrap();
}

#[cfg(unix)]
#[test]
fn launch_without_rex_records_a_failed_start_and_status_reads_it_back() {
    let state = prepared("delegate-no-rex");
    let prompt = state.join("prompt.txt");
    let launched = run(
        &state,
        &[
            "delegate",
            "launch",
            "--model",
            "local/vendor/model",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--server",
            "127.0.0.1:4400",
        ],
    );
    assert_eq!(
        launched.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&launched.stderr)
    );
    let stderr = String::from_utf8_lossy(&launched.stderr);
    assert!(stderr.contains("launch_failed"), "{stderr}");
    assert!(!stderr.contains("effects are uncertain"), "{stderr}");
    let assignments = state.join("delegation/assignments");
    let entries: Vec<_> = std::fs::read_dir(&assignments).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let id = entries[0]
        .as_ref()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    let status = run(&state, &["delegate", "status", &id]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let value: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["state"], "launch_failed");
    assert_eq!(value["settled"], false);
    assert!(value["claim"].is_null());
    assert!(!String::from_utf8_lossy(&status.stdout).contains("quartz-lantern"));
    let waited = run(&state, &["delegate", "status", &id, "--wait", "0"]);
    assert_eq!(waited.status.code(), Some(6));
    let cancelled = run(&state, &["delegate", "cancel", &id]);
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    let value: Value = serde_json::from_slice(&cancelled.stdout).unwrap();
    assert_eq!(value["state"], "cancelled");
    let again = run(&state, &["delegate", "cancel", &id]);
    assert!(again.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&again.stdout).unwrap()["state"],
        "cancelled"
    );
    let unknown = run(&state, &["delegate", "status", "not-an-id"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(run(&state, &["serve", "stop"]).status.success());
    std::fs::remove_dir(value["assignment"]["scratch"].as_str().unwrap()).unwrap();
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn the_worker_refuses_a_missing_assignment_without_output() {
    let state = fixture_directory::create_unique_fixture_directory("delegate-worker-cli");
    let missing = state.join("nowhere");
    let worker = run(
        &state,
        &[
            "internal-worker",
            "--assignment-dir",
            missing.to_str().unwrap(),
            "--attempt",
            "a1",
        ],
    );
    assert_eq!(worker.status.code(), Some(2));
    assert!(worker.stdout.is_empty());
    std::fs::remove_dir_all(state).unwrap();
}
