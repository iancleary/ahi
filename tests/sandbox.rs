//! `ahi sandbox check` through the real CLI: detection only, exit 0, and the
//! platform-specific evidence lines CI runners exercise on Linux and Windows.

use std::process::Command;

#[test]
fn check_prints_a_backend_and_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .args(["sandbox", "check"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.starts_with(&format!("platform: {}\n", std::env::consts::OS)),
        "{text}"
    );
    assert!(text.contains("\nbackend: "), "{text}");
    assert!(text.contains("not implemented yet"), "{text}");
    if cfg!(target_os = "linux") {
        assert!(text.contains("\nlandlock: "), "{text}");
        assert!(text.contains("\nbwrap: "), "{text}");
        assert!(text.contains("\nouter sandbox: "), "{text}");
    }
    if cfg!(target_os = "windows") {
        assert!(text.contains("backend: job object"), "{text}");
    }
}

#[test]
fn doctor_carries_the_same_summary() {
    let output = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .arg("doctor")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("\nsandbox: backend ") || text.contains("\nsandbox: outer sandbox detected"),
        "{text}"
    );
}
