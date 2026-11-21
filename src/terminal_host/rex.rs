//! Rex CLI/API contracts: <https://www.superlogical.com/rex/docs/reference/cli>
//! and <https://www.superlogical.com/rex/docs/reference/api>. Full IDs and an explicit server prevent focus/label routing.
//! Terminal process/creation payloads follow the native terminal block contracts.
use super::*;
use serde_json::json;

pub(super) fn new_session(server: &str, op: &Operation, timeout: Duration) -> Result<Vec<Call>> {
    let Operation::Start { argv, cwd } = op else {
        return Err(Error::input(
            "server-only target requires starting a new session",
        ));
    };
    let mut args = vec![
        "--autostart=false".into(),
        "--server".into(),
        server.into(),
        "--timeout".into(),
        format!("{}ms", timeout.as_millis()),
        "new".into(),
        "--json".into(),
        "--shell".into(),
        "none".into(),
        "--cwd".into(),
        cwd.clone(),
        "--".into(),
    ];
    args.extend_from_slice(argv);
    Ok(vec![Call {
        argv: args,
        stdin: None,
        response: Response::Json,
    }])
}
pub(super) fn created_session(server: &str, value: Value) -> Result<Reply> {
    #[derive(Deserialize)]
    struct Created {
        session_id: String,
        initial_windows: Vec<Window>,
    }
    #[derive(Deserialize)]
    struct Window {
        block_ids: Vec<String>,
    }
    let c: Created = super::decode(value)?;
    let [w] = c.initial_windows.as_slice() else {
        return Err(Error::protocol());
    };
    let [block] = w.block_ids.as_slice() else {
        return Err(Error::protocol());
    };
    let terminal = Handle {
        target: Target::Rex {
            server: server.into(),
            session: c.session_id,
        },
        id: block.clone(),
    };
    terminal.validate().map_err(|_| Error::protocol())?;
    Ok(Reply::Started { terminal })
}

pub(super) fn prepare(
    server: &str,
    session: &str,
    op: &Operation,
    timeout: Duration,
) -> Result<Vec<Call>> {
    let base = vec![
        "--autostart=false".into(),
        "--server".into(),
        server.into(),
        "--timeout".into(),
        format!("{}ms", timeout.as_millis().max(1)),
    ];
    let api = |method: &str, params: Value| {
        let mut argv = base.clone();
        argv.extend(["api".into(), "call".into(), method.into(), "-".into()]);
        Call {
            argv,
            stdin: Some(Input::Json(params)),
            response: Response::Json,
        }
    };
    let id = op.terminal().map(|terminal| terminal.id.as_str());
    let text = |text: &str| {
        let mut argv = base.clone();
        argv.extend([
            "send".into(),
            "--session".into(),
            session.into(),
            "--block".into(),
            id.unwrap_or_default().into(),
        ]);
        Call {
            argv,
            stdin: Some(Input::Text(text.into())),
            response: Response::Text,
        }
    };
    let key = |key: Key| {
        let mut argv = base.clone();
        argv.extend([
            "send-key".into(),
            "--session".into(),
            session.into(),
            "--block".into(),
            id.unwrap_or_default().into(),
            key.native().into(),
        ]);
        Call {
            argv,
            stdin: None,
            response: Response::Text,
        }
    };
    let calls = match op {
        Operation::Start { argv, cwd } => vec![api(
            "session.new_window",
            json!({
                "session_id": session, "focus": false,
                "layout": {"block": {"flavor": "com.superlogical.terminal.shell",
                    "options": {"shell": "none", "command": argv, "cwd": cwd, "exit": {"on_completion": false}}}}
            }),
        )],
        Operation::Inspect { .. } => vec![api(
            "com.superlogical.terminal.process",
            json!({"session_id": session, "block_id": id, "args": {}}),
        )],
        Operation::Capture { .. } => {
            let mut argv = base;
            argv.extend([
                "capture".into(),
                "--session".into(),
                session.into(),
                "--block".into(),
                id.unwrap_or_default().into(),
                "--format=text".into(),
            ]);
            vec![Call {
                argv,
                stdin: None,
                response: Response::Text,
            }]
        }
        Operation::Text { text: data, .. } => vec![text(data)],
        Operation::Submit { text: data, .. } => vec![text(data), key(Key::Enter)],
        Operation::Key { key: which, .. } => vec![key(*which)],
        Operation::Interrupt { .. } => vec![key(Key::ControlC)],
        Operation::Terminate { .. } => vec![api(
            "block.close",
            json!({"session_id": session, "block_id": id}),
        )],
    };
    Ok(calls)
}

pub(super) fn decode(target: &Target, op: &Operation, value: Value) -> Result<Reply> {
    if matches!(op, Operation::Start { .. }) {
        #[derive(Deserialize)]
        struct Created {
            block_ids: Vec<String>,
        }
        let Created { block_ids } = super::decode(value)?;
        let [id] = block_ids.as_slice() else {
            return Err(Error::protocol());
        };
        let terminal = Handle {
            target: target.clone(),
            id: id.clone(),
        };
        terminal.validate().map_err(|_| Error::protocol())?;
        return Ok(Reply::Started { terminal });
    }
    let terminal = op.terminal().ok_or_else(Error::protocol)?.clone();
    match op {
        Operation::Inspect { .. } => {
            // Missing evidence is not an exited process. Reject a different response
            // shape rather than turning an empty object into a replacement decision.
            if !["child", "foreground", "last_exit"]
                .iter()
                .any(|key| value.get(key).is_some())
            {
                return Err(Error::protocol());
            }
            #[derive(Deserialize)]
            struct Child {
                #[serde(rename = "pid")]
                _pid: std::num::NonZeroU32,
            }
            #[derive(Deserialize)]
            struct Exit {
                exit_code: i32,
                signal: Option<i32>,
            }
            #[derive(Deserialize)]
            struct Process {
                child: Option<Child>,
                foreground: Option<Child>,
                last_exit: Option<Exit>,
            }
            let process: Process = super::decode(value)?;
            let state = if process.child.is_some() || process.foreground.is_some() {
                ProcessState::Running
            } else if let Some(exit) = process.last_exit {
                ProcessState::Exited {
                    code: Some(exit.exit_code),
                    signal: exit.signal,
                }
            } else {
                ProcessState::Pending
            };
            Ok(Reply::Inspected {
                terminal,
                process: state,
            })
        }
        Operation::Capture { .. } => Ok(Reply::Captured {
            terminal,
            text: super::decode(value)?,
            extent: "screen_and_scrollback".into(),
            complete: None,
        }),
        Operation::Interrupt { .. } => Ok(Reply::Interrupted {
            terminal,
            key: "ctrl+c".into(),
            observation: None,
        }),
        Operation::Terminate { .. } => {
            if !value.is_object() {
                return Err(Error::protocol());
            }
            Ok(Reply::StopRequested {
                terminal,
                record_retained: false,
                process: None,
            })
        }
        Operation::Text { .. } | Operation::Submit { .. } | Operation::Key { .. } => {
            Ok(Reply::InputSent { terminal })
        }
        Operation::Start { .. } => Err(Error::protocol()),
    }
}
