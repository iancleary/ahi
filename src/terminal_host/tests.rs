use super::*;
use serde_json::json;

const SESSION: &str = "session:11111111-1111-4111-8111-111111111111";
const BLOCK: &str = "block:22222222-2222-4222-8222-222222222222";
fn rex_target() -> Target {
    Target::Rex {
        server: "work-wsl".into(),
        session: SESSION.into(),
    }
}
fn hc_target() -> Target {
    Target::Hc {
        session: "fixture".into(),
        machine: Some("work-windows".into()),
    }
}
fn terminal(target: &Target) -> Handle {
    Handle {
        target: target.clone(),
        id: match target {
            Target::Rex { .. } | Target::RexServer { .. } => BLOCK,
            Target::Hc { .. } => "work-windows/t7",
        }
        .into(),
    }
}
fn start() -> Operation {
    Operation::Start {
        argv: vec![
            "C:\\Program Files\\Example\\app.exe".into(),
            "$(not a shell); literal".into(),
        ],
        cwd: "C:\\work".into(),
    }
}

#[test]
fn start_preserves_argv_and_lifetime_without_focus_or_template_defaults() {
    let rex = Client::new(rex_target())
        .unwrap()
        .preview(&start())
        .unwrap();
    assert_eq!(
        rex["calls"][0]["argv"],
        json!([
            "--autostart=false",
            "--server",
            "work-wsl",
            "--timeout",
            "15000ms",
            "api",
            "call",
            "session.new_window",
            "-"
        ])
    );
    assert_eq!(
        rex["calls"][0]["stdin"],
        json!({"session_id": SESSION, "focus": false,
        "layout": {"block": {"flavor": "com.superlogical.terminal.shell", "options": {
            "shell": "none", "command": ["C:\\Program Files\\Example\\app.exe", "$(not a shell); literal"],
            "cwd": "C:\\work", "exit": {"on_completion": false}
        }}}})
    );
    let hc = Client::new(hc_target()).unwrap().preview(&start()).unwrap();
    assert_eq!(
        hc["calls"][0]["stdin"],
        json!({"jsonrpc":"2.0", "id":1, "method":"terminal.run", "params": {
            "argv":["C:\\Program Files\\Example\\app.exe", "$(not a shell); literal"], "cwd":"C:\\work", "on":"work-windows",
            "wait":false, "no_parent":true, "no_notify":true, "keep":false, "rm":false
        }})
    );
}

#[test]
fn handles_cannot_cross_backends_environments_or_use_labels() {
    let rex = Client::new(rex_target()).unwrap();
    assert_eq!(
        rex.preview(&Operation::Inspect {
            terminal: terminal(&hc_target())
        })
        .unwrap_err()
        .kind,
        ErrorKind::Input
    );
    let mut other = terminal(&rex_target());
    other.target = Target::Rex {
        server: "work-native-windows".into(),
        session: SESSION.into(),
    };
    assert!(rex
        .preview(&Operation::Terminate { terminal: other })
        .is_err());
    let mut label = terminal(&rex_target());
    label.id = "logs".into();
    assert!(rex
        .preview(&Operation::Terminate { terminal: label })
        .is_err());
    let mut hc = terminal(&hc_target());
    hc.id = "t7".into();
    assert!(Client::new(hc_target())
        .unwrap()
        .preview(&Operation::Inspect { terminal: hc })
        .is_err());
}

#[test]
fn input_and_stop_semantics_stay_backend_specific() {
    let hc = Client::new(hc_target()).unwrap();
    assert_eq!(
        hc.preview(&Operation::Text {
            terminal: terminal(&hc_target()),
            text: "draft".into()
        })
        .unwrap_err()
        .kind,
        ErrorKind::Unsupported
    );
    let submitted = hc
        .preview(&Operation::Submit {
            terminal: terminal(&hc_target()),
            text: "draft".into(),
        })
        .unwrap();
    assert_eq!(
        submitted["calls"][0]["stdin"]["params"],
        json!({"terminal":"work-windows/t7", "text":"draft", "force":false, "wait":false})
    );
    let rex = Client::new(rex_target()).unwrap();
    let submitted = rex
        .preview(&Operation::Submit {
            terminal: terminal(&rex_target()),
            text: "draft".into(),
        })
        .unwrap();
    assert_eq!(submitted["calls"].as_array().unwrap().len(), 2);
    assert_eq!(submitted["calls"][0]["stdin"], "draft");
    assert_eq!(
        submitted["calls"][1]["argv"],
        json!([
            "--autostart=false",
            "--server",
            "work-wsl",
            "--timeout",
            "15000ms",
            "send-key",
            "--session",
            SESSION,
            "--block",
            BLOCK,
            "enter"
        ])
    );
    let stop = rex
        .preview(&Operation::Terminate {
            terminal: terminal(&rex_target()),
        })
        .unwrap();
    assert_eq!(
        stop["calls"][0]["stdin"],
        json!({"session_id":SESSION, "block_id":BLOCK})
    );
    assert_eq!(stop["calls"][0]["argv"][7], "block.close");
}

#[test]
fn process_evidence_never_turns_missing_contact_into_exit() {
    let target = rex_target();
    let op = Operation::Inspect {
        terminal: terminal(&target),
    };
    for (value, expected) in [
        (
            json!({"child":null,"foreground":null,"last_exit":null}),
            ProcessState::Pending,
        ),
        (
            json!({"child":{"pid":42},"foreground":null,"last_exit":null}),
            ProcessState::Running,
        ),
        (
            json!({"child":null,"foreground":null,"last_exit":{"exit_code":7}}),
            ProcessState::Exited {
                code: Some(7),
                signal: None,
            },
        ),
    ] {
        let Reply::Inspected { process, .. } = rex::decode(&target, &op, value).unwrap() else {
            panic!("wrong reply")
        };
        assert_eq!(process, expected);
    }
    assert!(rex::decode(&target, &op, json!({})).is_err());
    assert!(rex::decode(
        &target,
        &op,
        json!({"child":null,"foreground":null,"last_exit":{}})
    )
    .is_err());
    assert!(rex::decode(
        &target,
        &op,
        json!({"child":{"pid":0},"foreground":null,"last_exit":{"exit_code":0}})
    )
    .is_err());
    let target = hc_target();
    let op = Operation::Inspect {
        terminal: terminal(&target),
    };
    let Reply::Inspected { process, .. } = hc::decode(
        &target,
        &op,
        json!({"address":"work-windows/t7", "state":"finished", "reason":"holder_lost"}),
    )
    .unwrap() else {
        panic!("wrong reply")
    };
    assert_eq!(
        process,
        ProcessState::Unknown {
            reason: "holder_lost".into()
        }
    );
    assert!(hc::decode(
        &target,
        &op,
        json!({"address":"work-windows/t8", "state":"live"})
    )
    .is_err());
}

#[test]
fn snapshots_preserve_incompleteness_and_interrupts_do_not_claim_exit() {
    let target = hc_target();
    let reply = hc::decode(
        &target,
        &Operation::Capture {
            terminal: terminal(&target),
        },
        json!({"text":"hello\n", "complete":false, "how":"screen", "why":"working"}),
    )
    .unwrap();
    let value = serde_json::to_value(reply).unwrap();
    assert_eq!(value["text"], "hello\n");
    assert_eq!(value["complete"], false);
    let target = rex_target();
    let reply = rex::decode(
        &target,
        &Operation::Interrupt {
            terminal: terminal(&target),
        },
        json!(""),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(reply).unwrap()["observation"],
        Value::Null
    );
}

#[test]
fn native_process_boundary_is_bounded_and_never_retries_mutations() {
    use std::fs;
    let root = crate::fixture_directory::create_unique_fixture_directory("terminal-host-contract");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let executable = root.join(format!("backend{}", std::env::consts::EXE_SUFFIX));
    let compile = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "tests/support/terminal_host_backend.rs",
            "-o",
        ])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let target = hc_target();
    let client = Client::new(target.clone())
        .unwrap()
        .with_executable(executable.clone());
    fs::write(root.join("reply"), "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"address\":\"work-windows/t7\",\"state\":\"live\"}}\n").unwrap();
    let Reply::Started { terminal: started } = client.execute(&start()).unwrap() else {
        panic!("wrong reply")
    };
    assert_eq!(started, terminal(&target));
    let input: Value = serde_json::from_slice(&fs::read(root.join("input")).unwrap()).unwrap();
    assert_eq!(input["method"], "terminal.run");
    assert_eq!(input["params"]["argv"][1], "$(not a shell); literal");
    fs::write(
        root.join("reply"),
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n",
    )
    .unwrap();
    let error = client.execute(&start()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.effects_uncertain);
    assert_eq!(
        fs::read_to_string(root.join("calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );

    let target = rex_target();
    let client = Client::new(target.clone())
        .unwrap()
        .with_executable(executable.clone());
    fs::write(root.join("reply"), "{}").unwrap();
    fs::write(root.join("mode"), "fail-key").unwrap();
    let error = client
        .execute(&Operation::Submit {
            terminal: terminal(&target),
            text: "once".into(),
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Backend);
    assert!(error.effects_uncertain);
    assert_eq!(
        fs::read_to_string(root.join("calls"))
            .unwrap()
            .lines()
            .count(),
        4
    );

    assert_eq!(fs::read(root.join("input")).unwrap(), b"once");

    fs::write(root.join("mode"), "flood").unwrap();
    assert_eq!(
        client
            .execute(&Operation::Capture {
                terminal: terminal(&target)
            })
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    fs::write(root.join("mode"), "hang").unwrap();
    let client = Client::new(hc_target())
        .unwrap()
        .with_executable(executable)
        .with_timeout(Duration::from_millis(100))
        .unwrap();
    let began = std::time::Instant::now();
    let error = client
        .execute(&Operation::Submit {
            terminal: terminal(&hc_target()),
            // JSON escaping exceeds pipe capacity while the fixture never reads.
            text: "\u{1}".repeat(16 * 1024),
        })
        .unwrap_err();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "connector cleanup exceeded its bound"
    );
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(error.effects_uncertain);
    let missing = Client::new(target)
        .unwrap()
        .with_executable(root.join("not-installed"));
    let error = missing.execute(&start()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(!error.effects_uncertain);
}
