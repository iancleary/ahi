//! `ahi exec` and `ahi serve` through the shipped binary against a loopback
//! gateway: prompt sources, the result document, exit codes, tools under
//! policy, run_id replay, timeouts, and a turn over the socket. No real
//! credentials, no external network.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

const STREAM: &str = concat!(
    ": keepalive\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7}}\n\n",
    "data: [DONE]\n\n"
);

/// Serve one response per connection, in order, and hand back every request.
fn loopback(responses: Vec<(&str, &str, String)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let responses: Vec<String> = responses
        .into_iter()
        .map(|(status, content_type, body)| {
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        })
        .collect();
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        responses
            .iter()
            .map(|response| serve_one(&listener, response))
            .collect()
    });
    (url, handle)
}

fn stream(body: &str) -> (&'static str, &'static str, String) {
    ("200 OK", "text/event-stream", body.to_string())
}

fn serve_one(listener: &TcpListener, response: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "no model request reached the gateway"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "request cut short");
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        request.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    request.push_str(std::str::from_utf8(&body).unwrap());
    let mut stream = stream;
    stream.write_all(response.as_bytes()).unwrap();
    request
}

/// A Chat Completions stream requesting `calls`, then one that answers.
fn tool_round(
    calls: &[(&str, &str, serde_json::Value)],
    answer: &str,
) -> Vec<(&'static str, &'static str, String)> {
    let deltas: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (id, name, arguments))| {
            serde_json::json!({"index": index, "id": id, "type": "function",
                   "function": {"name": name, "arguments": arguments.to_string()}})
        })
        .collect();
    let first = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": deltas}}]}),
        serde_json::json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
    );
    let second = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"choices": [{"index": 0, "delta": {"content": answer}, "finish_reason": "stop"}]})
    );
    vec![stream(&first), stream(&second)]
}

/// A fixture root: `state/` curates `local/vendor/model` at `url`, `work/`
/// is the tool root, `ops/` holds operator files beside it.
fn fixture(url: &str) -> PathBuf {
    let dir = fixture_directory::create_unique_fixture_directory("exec");
    fs::create_dir(dir.join("state")).unwrap();
    fs::write(
        dir.join("state/models.json"),
        format!(
            r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"gateway","models":[{{"id":"vendor/model"}}]}}}}}}"#
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("work/src")).unwrap();
    fs::write(dir.join("work/src/a.rs"), "fn a() {}\n").unwrap();
    fs::create_dir(dir.join("ops")).unwrap();
    dir
}

fn ahi(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ahi"));
    command
        .arg("--state-dir")
        .arg(dir.join("state"))
        .current_dir(dir.join("work"))
        .stdin(Stdio::null());
    command
}

fn exec(dir: &Path, args: &[&str]) -> Output {
    let mut command = ahi(dir);
    command.args(["exec", "--model", "local/vendor/model", "--standalone"]);
    command.args(args);
    command.output().expect("ahi exec")
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn sessions(dir: &Path) -> Vec<serde_json::Value> {
    let listed = ahi(dir)
        .args(["session", "list", "--json"])
        .output()
        .unwrap();
    out(&listed)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_prompt_turn_prints_the_answer_and_is_recorded() {
    let (url, gateway) = loopback(vec![stream(STREAM)]);
    let dir = fixture(&url);
    let output = exec(&dir, &["-p", "say hello"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert_eq!(out(&output), "Hello 世界\n");
    assert_eq!(err(&output), "", "nothing on stderr without -v");
    let requests = gateway.join().unwrap();
    assert!(requests[0].contains("\"say hello\""));
    let rows = sessions(&dir);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["last_status"], "ok");
    assert_eq!(rows[0]["entries"], 3, "operator, assistant, turn");
    assert_eq!(
        rows[0]["cwd"],
        dir.join("work")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );

    // --format json prints the document; a second turn on the same session
    // sees the first exchange.
    let (url, gateway) = loopback(vec![stream(STREAM)]);
    fs::write(
        dir.join("state/models.json"),
        format!(
            r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"gateway","models":[{{"id":"vendor/model"}}]}}}}}}"#
        ),
    )
    .unwrap();
    let id = rows[0]["id"].as_str().unwrap();
    let output = exec(
        &dir,
        &["-p", "again", "--session", id, "--format", "json", "-v"],
    );
    assert_eq!(code(&output), 0, "{}", err(&output));
    let doc: serde_json::Value = serde_json::from_str(&out(&output)).unwrap();
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["text"], "Hello 世界");
    assert_eq!(doc["effects"], "none");
    assert_eq!(doc["origin"], "exec");
    assert_eq!(doc["session"], id);
    assert_eq!(doc["usage"]["input_tokens"], 42);
    assert!(err(&output).contains("started"), "{}", err(&output));
    let requests = gateway.join().unwrap();
    assert!(requests[0].contains("\"say hello\"") && requests[0].contains("\"again\""));
    assert_eq!(sessions(&dir)[0]["turns"], 2);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn headless_takes_files_outside_the_workdir_and_writes_the_document_once() {
    let (url, gateway) = loopback(vec![stream(STREAM)]);
    let dir = fixture(&url);
    let prompt = dir.join("ops/task.md");
    fs::write(&prompt, "do the thing\n").unwrap();
    let result = dir.join("ops/result.json");
    let bad = exec(
        &dir,
        &[
            "--headless",
            "-p",
            "x",
            "--result-file",
            result.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&bad), 2);
    assert!(err(&bad).contains("--prompt-file"));
    let no_result = exec(
        &dir,
        &["--headless", "--prompt-file", prompt.to_str().unwrap()],
    );
    assert_eq!(code(&no_result), 2);
    fs::write(dir.join("work/inside.md"), "x").unwrap();
    let inside = exec(
        &dir,
        &[
            "--headless",
            "--prompt-file",
            dir.join("work/inside.md").to_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&inside), 2);
    assert!(err(&inside).contains("inside the working directory"));
    let mismatch = exec(
        &dir,
        &[
            "--headless",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
            "--prompt-sha256",
            "00",
        ],
    );
    assert_eq!(code(&mismatch), 2);
    assert!(!result.exists(), "no document for a refused run");
    assert!(
        sessions(&dir).is_empty(),
        "nothing recorded for refused runs"
    );

    let digest = {
        use sha2::Digest;
        sha2::Sha256::digest(b"do the thing\n")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let output = exec(
        &dir,
        &[
            "--headless",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
            "--prompt-sha256",
            &digest,
            "--run-id",
            "task-1",
        ],
    );
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert_eq!(out(&output), "Hello 世界\n");
    assert!(err(&output).contains("run task-1"), "{}", err(&output));
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&result).unwrap()).unwrap();
    assert_eq!(doc["run_id"], "task-1");
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["prompt_sha256"], digest);
    assert_eq!(doc["tools"]["calls"], 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&result).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    gateway.join().unwrap();

    // The same run id again is answered from the record, no gateway needed;
    // the result file is refused because it exists.
    let existing = exec(
        &dir,
        &[
            "--headless",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
            "--run-id",
            "task-1",
        ],
    );
    assert_eq!(code(&existing), 2);
    assert!(err(&existing).contains("already exists"));
    let id = sessions(&dir)[0]["id"].as_str().unwrap().to_string();
    let replayed = exec(
        &dir,
        &[
            "-p",
            "do the thing\n",
            "--run-id",
            "task-1",
            "--session",
            &id,
            "--format",
            "json",
            "-v",
        ],
    );
    assert_eq!(code(&replayed), 0, "{}", err(&replayed));
    let doc: serde_json::Value = serde_json::from_str(&out(&replayed)).unwrap();
    assert_eq!(doc["text"], "Hello 世界");
    assert!(err(&replayed).contains("from the record"));
    let changed = exec(
        &dir,
        &[
            "-p",
            "something else",
            "--run-id",
            "task-1",
            "--session",
            &id,
        ],
    );
    assert_eq!(code(&changed), 7, "{}", err(&changed));
    assert!(err(&changed).contains("different prompt"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tools_run_under_policy_and_an_ask_is_denied_headless() {
    let (url, gateway) = loopback(tool_round(
        &[
            ("c1", "read_file", serde_json::json!({"path": "src/a.rs"})),
            (
                "c2",
                "write_file",
                serde_json::json!({"path": "src/new.rs", "content": "// new\n"}),
            ),
            ("c3", "run_command", serde_json::json!({"argv": ["true"]})),
        ],
        "done",
    ));
    let dir = fixture(&url);
    let policy = dir.join("ops/task.tools.yaml");
    fs::write(
        &policy,
        "version: 1\nrules:\n\
         - {id: reads, action: tool_call, match: {tool: read_file, path: src}, effect: approve}\n\
         - {id: writes, action: file_write, match: {tool: write_file, path: src}, effect: approve, ask: true}\n\
         - {id: noop, action: command, match: {argv: ['true']}, effect: approve}\n",
    )
    .unwrap();
    let prompt = dir.join("ops/task.md");
    fs::write(&prompt, "read and write").unwrap();
    let result = dir.join("ops/result.json");
    let output = exec(
        &dir,
        &[
            "--headless",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
            "--tools",
            policy.to_str().unwrap(),
            "--run-id",
            "tools-1",
        ],
    );
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert_eq!(out(&output), "done\n");
    let progress = err(&output);
    assert!(
        progress.contains("tool read_file src/a.rs exit 0"),
        "{progress}"
    );
    assert!(
        progress.contains("tool write_file src/new.rs exit 126"),
        "{progress}"
    );
    assert!(progress.contains("tool run_command . exit 0"), "{progress}");
    assert!(!progress.contains("// new"), "no file contents on stderr");
    assert!(!dir.join("work/src/new.rs").exists(), "the ask was denied");
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&result).unwrap()).unwrap();
    assert_eq!(doc["tools"]["calls"], 3);
    assert_eq!(doc["tools"]["ask_denied"], 1);
    assert_eq!(doc["tools"]["denied"], 1);
    assert_eq!(doc["effects"], "some");
    let ledger = doc["ledger"].as_array().unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0]["tool"], "run_command");
    assert_eq!(ledger[0]["ran"], true);
    let requests = gateway.join().unwrap();
    assert!(
        requests[1].contains("no operator is attending"),
        "the model is told why: {}",
        requests[1]
    );
    let audit = fs::read_to_string(dir.join("state/audit.log")).unwrap();
    let line: serde_json::Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(line["origin"], "exec");
    assert_eq!(line["run_id"], "tools-1");

    // A policy file inside the workdir, and a bad one, are refused.
    let inside = dir.join("work/p.yaml");
    fs::write(&inside, "version: 1\nrules: []\n").unwrap();
    let refused = exec(&dir, &["-p", "x", "--tools", inside.to_str().unwrap()]);
    assert_eq!(code(&refused), 2);
    fs::write(&policy, "nope: [").unwrap();
    let invalid = exec(&dir, &["-p", "x", "--tools", policy.to_str().unwrap()]);
    assert_eq!(code(&invalid), 5, "{}", err(&invalid));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_timeout_closes_the_turn_as_timeout_with_exit_6_and_check_validates() {
    // A gateway that accepts and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let silent = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(4));
        drop(stream);
    });
    let dir = fixture(&url);
    let result = dir.join("ops/result.json");
    let started = Instant::now();
    let output = exec(
        &dir,
        &[
            "-p",
            "slow",
            "--timeout",
            "1",
            "--result-file",
            result.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&output), 6, "{}", err(&output));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(out(&output).trim(), "", "nothing on stdout for text");
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&result).unwrap()).unwrap();
    assert_eq!(doc["status"], "timeout");
    assert_eq!(doc["effects"], "none");
    assert_eq!(sessions(&dir)[0]["last_status"], "timeout");
    silent.join().unwrap();

    let ok = exec(&dir, &["--check", "-p", "ignored"]);
    assert_eq!(code(&ok), 0, "{}", err(&ok));
    assert!(err(&ok).contains("ok:"));
    let unknown = ahi(&dir)
        .args(["exec", "--model", "local/other", "--standalone", "--check"])
        .output()
        .unwrap();
    assert_eq!(code(&unknown), 3, "{}", err(&unknown));
    let no_prompt = exec(&dir, &[]);
    assert_eq!(code(&no_prompt), 2);
    assert!(
        err(&no_prompt).contains("prompt is empty"),
        "{}",
        err(&no_prompt)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn the_server_runs_a_turn_over_the_socket_and_connect_bridges_stdio() {
    let (url, gateway) = loopback(vec![stream(STREAM)]);
    let dir = fixture(&url);
    let socket = std::env::temp_dir().join(format!("ahi-test-{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket);
    let mut server = ahi(&dir)
        .args(["serve", "--socket", socket.to_str().unwrap()])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "the server did not come up");
        thread::sleep(Duration::from_millis(20));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // A second server on the same socket is refused.
    let second = ahi(&dir)
        .args(["serve", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&second), 1);
    assert!(err(&second).contains("already runs"), "{}", err(&second));
    assert!(
        socket.with_extension("lock").exists(),
        "the lock sits beside the socket"
    );

    // exec finds the server and runs its turn there.
    let output = ahi(&dir)
        .args([
            "exec",
            "--model",
            "local/vendor/model",
            "-p",
            "over the socket",
            "-v",
            "--socket",
        ])
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert_eq!(out(&output), "Hello 世界\n");
    assert!(
        err(&output).contains("using the server"),
        "{}",
        err(&output)
    );
    gateway.join().unwrap();
    let rows = sessions(&dir);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["last_status"], "ok");
    let turned = rows[0]["id"].as_str().unwrap().to_string();

    // Caps: 64 connections, then a refusal; 16 subscriptions per connection.
    // The kernel accepts every connect at once; the server seats them in
    // order and refuses the one past the cap, which may be any of them if a
    // seat from the exec above is still being given back, so each is read
    // with a short timeout and exactly one carries the refusal.
    let seats: Vec<std::os::unix::net::UnixStream> = (0..65)
        .map(|_| {
            let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            stream
        })
        .collect();
    let mut refusals = 0;
    for seat in &seats {
        let mut line = String::new();
        if BufReader::new(seat).read_line(&mut line).is_ok() && !line.is_empty() {
            let refusal: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(refusal["error"]["kind"], "limit", "{line}");
            refusals += 1;
        }
    }
    assert_eq!(refusals, 1, "one connection past the cap is refused");
    drop(seats);
    let mut one = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    let mut lines = BufReader::new(one.try_clone().unwrap()).lines();
    let created: serde_json::Value = {
        writeln!(
            one,
            r#"{{"id": 1, "method": "session.create", "params": {{"cwd": "{}", "gateway": "local", "model": "vendor/model", "thinking": "off"}}}}"#,
            dir.join("work").display()
        )
        .unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    let sid = created["result"]["id"].as_str().unwrap().to_string();
    let mut kinds = Vec::new();
    for n in 0..17 {
        writeln!(
            one,
            r#"{{"id": {n}, "method": "session.subscribe", "params": {{"session": "{sid}", "after": 0}}}}"#
        )
        .unwrap();
        loop {
            let v: serde_json::Value =
                serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            if v.get("event").is_none() {
                kinds.push(v["error"]["kind"].as_str().map(str::to_string));
                break;
            }
        }
    }
    assert!(kinds[..16].iter().all(Option::is_none), "{kinds:?}");
    assert_eq!(kinds[16].as_deref(), Some("limit"));
    drop(lines);
    drop(one);

    // connect bridges stdin and stdout to the socket: one request, one answer,
    // then a subscription whose replayed entries arrive as events.
    let id = turned.as_str();
    let mut bridge = ahi(&dir)
        .args(["connect", "--socket", socket.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = bridge.stdin.take().unwrap();
    writeln!(stdin, r#"{{"id": 7, "method": "health", "params": {{}}}}"#).unwrap();
    writeln!(
        stdin,
        r#"{{"id": 8, "method": "session.subscribe", "params": {{"session": "{id}", "after": 0}}}}"#
    )
    .unwrap();
    let mut lines = BufReader::new(bridge.stdout.take().unwrap()).lines();
    let health: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(health["id"], 7);
    assert_eq!(health["result"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(health["result"]["open_turns"], 0);
    let subscribed: serde_json::Value =
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(subscribed["id"], 8);
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let event: serde_json::Value =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(event["event"], "entry");
        kinds.push(event["entry"]["type"].as_str().unwrap().to_string());
    }
    assert_eq!(kinds, ["message", "message", "turn"]);
    drop(stdin);
    assert!(bridge.wait().unwrap().success());

    // A bridge still attached when the server stops exits 3, so a
    // coordinator can tell "my stdin ended" from "the server went away".
    let mut attached = ahi(&dir)
        .args(["connect", "--socket", socket.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut attached_stdin = attached.stdin.take().unwrap();
    writeln!(
        attached_stdin,
        r#"{{"id": 1, "method": "health", "params": {{}}}}"#
    )
    .unwrap();
    let mut attached_lines = BufReader::new(attached.stdout.take().unwrap()).lines();
    let answered: serde_json::Value =
        serde_json::from_str(&attached_lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(answered["id"], 1);

    // stop ends the server cleanly and removes the socket.
    let stop = ahi(&dir)
        .args(["serve", "stop", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&stop), 0, "{}", err(&stop));
    let attached_status = attached.wait().unwrap();
    assert_eq!(attached_status.code(), Some(3), "the server closed first");
    drop(attached_stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = server.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the server did not stop");
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    assert!(!socket.exists());
    let mut log = String::new();
    server
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut log)
        .unwrap();
    assert!(
        log.contains("listening on") && log.contains("stopping on shutdown request"),
        "{log}"
    );
    let gone = ahi(&dir)
        .args(["serve", "stop", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&gone), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn serve_start_brings_up_one_detached_server_that_exec_then_uses() {
    let (url, gateway) = loopback(vec![stream(STREAM)]);
    let dir = fixture(&url);
    let socket = std::env::temp_dir().join(format!("ahi-start-{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket);
    let started = ahi(&dir)
        .args(["serve", "start", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&started), 0, "{}", err(&started));
    assert_eq!(out(&started).trim(), socket.to_str().unwrap());
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
    let again = ahi(&dir)
        .args(["serve", "start", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&again), 0);
    assert_eq!(out(&again), out(&started), "the running server is reused");
    let log = fs::read_to_string(dir.join("state/serve.log")).unwrap();
    assert_eq!(log.matches("listening on").count(), 1, "{log}");

    let output = ahi(&dir)
        .args([
            "exec",
            "--model",
            "local/vendor/model",
            "-p",
            "hi",
            "--socket",
        ])
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert_eq!(out(&output), "Hello 世界\n");
    gateway.join().unwrap();

    let stop = ahi(&dir)
        .args(["serve", "stop", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&stop), 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while socket.exists() {
        assert!(Instant::now() < deadline, "the server did not stop");
        thread::sleep(Duration::from_millis(20));
    }
    fs::remove_dir_all(dir).unwrap();
}

/// A coordinator's turn hands a task to a worker: the worker is another
/// `ahi serve` reached through the connect command `AHI_DELEGATE_VIA`
/// names, here `ahi connect` to its socket.
#[cfg(unix)]
#[test]
fn a_coordinator_delegates_a_task_to_a_worker_server() {
    // One gateway, three turns in order: the coordinator asks to delegate,
    // the worker answers, the coordinator concludes.
    let (url, gateway) = loopback(
        tool_round(
            &[(
                "c1",
                "delegate",
                serde_json::json!({"worker": "docs", "prompt": "write the summary"}),
            )],
            "delegated and done",
        )
        .into_iter()
        .take(1)
        .chain(vec![
            stream(STREAM),
            tool_round(&[], "delegated and done").pop().unwrap(),
        ])
        .collect(),
    );
    let coordinator = fixture(&url);
    let worker = fixture(&url);
    fs::write(worker.join("state/selection"), "local/vendor/model\n").unwrap();
    let socket = std::env::temp_dir().join(format!("ahi-worker-{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket);
    let mut server = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .arg("--state-dir")
        .arg(worker.join("state"))
        .args(["serve", "--socket", socket.to_str().unwrap()])
        .current_dir(worker.join("work"))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(
            Instant::now() < deadline,
            "the worker server did not come up"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let policy = coordinator.join("ops/tools.yaml");
    fs::write(
        &policy,
        "version: 1\nrules:\n- {id: docs, action: delegate, match: {worker: docs}, effect: approve}\n",
    )
    .unwrap();
    let prompt = coordinator.join("ops/task.md");
    fs::write(&prompt, "get the docs written").unwrap();
    let result = coordinator.join("ops/result.json");
    let via = format!(
        "{} --state-dir {} connect --socket {}",
        env!("CARGO_BIN_EXE_ahi"),
        worker.join("state").display(),
        socket.display()
    );
    let output = ahi(&coordinator)
        .env("AHI_DELEGATE_VIA", &via)
        .args([
            "exec",
            "--model",
            "local/vendor/model",
            "--standalone",
            "--headless",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--result-file",
            result.to_str().unwrap(),
            "--tools",
            policy.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert_eq!(out(&output), "delegated and done\n");
    assert!(
        err(&output).contains("tool delegate docs exit 0"),
        "{}",
        err(&output)
    );
    let requests = gateway.join().unwrap();
    assert!(
        requests[1].contains("write the summary"),
        "the worker got the task"
    );
    assert!(
        requests[2].contains("task ")
            && requests[2].contains("on docs: ok")
            && requests[2].contains("effects: none"),
        "the coordinator's model sees the worker's answer and effects: {}",
        requests[2]
    );
    assert!(requests[2].contains("Hello 世界"));
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&result).unwrap()).unwrap();
    assert_eq!(doc["tools"]["calls"], 1);
    assert_eq!(doc["effects"], "none", "the worker changed nothing");
    assert_eq!(doc["ledger"][0]["tool"], "delegate");
    assert_eq!(doc["ledger"][0]["worker"], "docs");
    assert_eq!(doc["ledger"][0]["status"], "ok");
    let worker_sessions = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .arg("--state-dir")
        .arg(worker.join("state"))
        .args(["session", "list", "--json"])
        .output()
        .unwrap();
    let row: serde_json::Value = serde_json::from_str(out(&worker_sessions).trim()).unwrap();
    assert_eq!(
        row["id"], doc["ledger"][0]["session"],
        "the task id is the worker session"
    );
    assert_eq!(
        row["cwd"],
        worker
            .join("work")
            .canonicalize()
            .unwrap()
            .display()
            .to_string(),
        "the worker session lives where its server runs"
    );
    let audit = fs::read_to_string(coordinator.join("state/audit.log")).unwrap();
    assert!(audit.contains("\"tool\":\"delegate\"") && audit.contains("\"worker\":\"docs\""));
    assert!(
        !audit.contains("write the summary"),
        "the task text is not logged"
    );

    let stop = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .arg("--state-dir")
        .arg(worker.join("state"))
        .args(["serve", "stop", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&stop), 0);
    server.wait().unwrap();
    fs::remove_dir_all(coordinator).unwrap();
    fs::remove_dir_all(worker).unwrap();
}

#[cfg(unix)]
#[test]
fn an_ask_nobody_hears_is_answered_unattended_and_the_turn_closes() {
    use std::os::unix::net::UnixStream;
    let (url, gateway) = loopback(tool_round(
        &[(
            "c1",
            "write_file",
            serde_json::json!({"path": "src/new.rs", "content": "// new\n"}),
        )],
        "done",
    ));
    let dir = fixture(&url);
    let policy = dir.join("ops/ask.tools.yaml");
    fs::write(
        &policy,
        "version: 1\nrules:\n\
         - {id: writes, action: file_write, match: {tool: write_file, path: src}, effect: approve, ask: true}\n",
    )
    .unwrap();
    let socket = std::env::temp_dir().join(format!("ahi-ask-{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket);
    let mut server = ahi(&dir)
        .args(["serve", "--socket", socket.to_str().unwrap()])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "the server did not come up");
        thread::sleep(Duration::from_millis(20));
    }
    let call = |stream: &mut UnixStream,
                lines: &mut std::io::Lines<BufReader<UnixStream>>,
                method: &str,
                params: serde_json::Value|
     -> serde_json::Value {
        writeln!(
            stream,
            "{}",
            serde_json::json!({"id": 1, "method": method, "params": params})
        )
        .unwrap();
        loop {
            let line: serde_json::Value =
                serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            if line.get("event").is_none() {
                assert!(line["error"].is_null(), "{line}");
                return line["result"].clone();
            }
        }
    };

    // An attended client subscribes, submits a turn that asks, hears the ask,
    // and goes away without answering.
    let mut stream = UnixStream::connect(&socket).unwrap();
    let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
    let work = dir.join("work").display().to_string();
    let header = call(
        &mut stream,
        &mut lines,
        "session.create",
        serde_json::json!({"cwd": work, "gateway": "local", "model": "vendor/model", "thinking": "off"}),
    );
    let session = header["id"].as_str().unwrap().to_string();
    call(
        &mut stream,
        &mut lines,
        "session.subscribe",
        serde_json::json!({"session": session, "after": 0}),
    );
    let submitted = call(
        &mut stream,
        &mut lines,
        "turn.submit",
        serde_json::json!({
            "session": session, "prompt": "write it", "run_id": "ask-1", "workdir": work,
            "tools": policy.to_str().unwrap(), "timeout": 30, "unattended": false,
            "origin": "exec", "client": "test",
        }),
    );
    assert_eq!(submitted["status"], "running");
    loop {
        let line: serde_json::Value =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        if line["event"] == "ask" {
            assert_eq!(line["run_id"], "ask-1");
            assert_eq!(line["calls"][0]["tool"], "write_file");
            break;
        }
    }
    drop(lines);
    drop(stream);

    // Another client sees the turn close on its own, denied as unattended.
    let mut other = UnixStream::connect(&socket).unwrap();
    let mut other_lines = BufReader::new(other.try_clone().unwrap()).lines();
    let deadline = Instant::now() + Duration::from_secs(15);
    let doc = loop {
        let found = call(
            &mut other,
            &mut other_lines,
            "turn.get",
            serde_json::json!({"session": session, "run_id": "ask-1"}),
        );
        if found["status"] != "running" {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "the turn stayed open with nobody to ask"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(doc["status"], "ok", "{doc}");
    assert_eq!(doc["tools"]["ask_denied"], 1, "{doc}");
    assert!(!dir.join("work/src/new.rs").exists(), "the ask was denied");
    gateway.join().unwrap();
    let info = call(
        &mut other,
        &mut other_lines,
        "session.get",
        serde_json::json!({"session": session}),
    );
    assert!(info["open_run_id"].is_null(), "{info}");

    // The server holds the session file: export reads it, import is refused.
    let exported = dir.join("ops/held.jsonl");
    let export = ahi(&dir)
        .args([
            "session",
            "export",
            &session,
            "--to",
            exported.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&export), 0, "{}", err(&export));
    let import = ahi(&dir)
        .args(["session", "import", exported.to_str().unwrap()])
        .output()
        .unwrap();
    assert_ne!(code(&import), 0);
    assert!(
        err(&import).contains("held by another process"),
        "{}",
        err(&import)
    );

    let stop = ahi(&dir)
        .args(["serve", "stop", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&stop), 0, "{}", err(&stop));
    assert!(server.wait().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn exec_exits_6_when_the_service_never_reports_the_turn() {
    use std::os::unix::net::UnixListener;
    let dir = fixture("http://127.0.0.1:1/");
    let socket = std::env::temp_dir().join(format!("ahi-silent-{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    // A server that accepts the turn and then says nothing more.
    let fake = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut methods = Vec::new();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            let result = match method.as_str() {
                "health" => {
                    serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "open_turns": 0})
                }
                "session.create" => serde_json::json!({"id": "fake-session"}),
                "session.get" => serde_json::json!({"id": "fake-session", "tip": 0}),
                "turn.submit" => serde_json::json!({"status": "running", "run_id": "silent-1"}),
                _ => serde_json::json!({}),
            };
            methods.push(method);
            writeln!(
                writer,
                "{}",
                serde_json::json!({"id": request["id"], "result": result})
            )
            .unwrap();
        }
        methods
    });
    let started = Instant::now();
    let output = ahi(&dir)
        .args([
            "exec",
            "--model",
            "local/vendor/model",
            "--timeout",
            "1",
            "-p",
            "anyone there",
            "--socket",
        ])
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(code(&output), 6, "{}", err(&output));
    assert!(
        err(&output).contains("run silent-1 did not close in time"),
        "{}",
        err(&output)
    );
    // The turn's timeout plus the service's two graces, and not much more.
    assert!(started.elapsed() >= Duration::from_secs(9));
    assert!(started.elapsed() < Duration::from_secs(20));
    let methods = fake.join().unwrap();
    assert!(methods.contains(&"turn.submit".to_string()), "{methods:?}");
    let _ = fs::remove_file(&socket);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_skill_load_is_in_the_result_the_session_and_the_audit_log() {
    let (url, gateway) = loopback(tool_round(
        &[(
            "c1",
            "load_skill",
            serde_json::json!({"name": "rust-review"}),
        )],
        "reviewed",
    ));
    let dir = fixture(&url);
    let skill = dir.join("work/.ahi/skills/rust-review");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: rust-review\ndescription: Review Rust changes\n---\n# Review\nLook twice.\n",
    )
    .unwrap();
    let policy = dir.join("ops/skills.tools.yaml");
    fs::write(
        &policy,
        "version: 1\nrules:\n\
         - {id: review, action: skill, match: {skill: rust-review}, effect: approve}\n",
    )
    .unwrap();
    let output = exec(
        &dir,
        &[
            "-p",
            "review it",
            "--tools",
            policy.to_str().unwrap(),
            "--run-id",
            "skill-1",
            "--format",
            "json",
        ],
    );
    assert_eq!(code(&output), 0, "{}", err(&output));
    let doc: serde_json::Value = serde_json::from_str(&out(&output)).unwrap();
    assert_eq!(doc["text"], "reviewed");
    assert_eq!(doc["tools"]["calls"], 1);
    assert_eq!(doc["tools"]["skills"], 1);
    assert_eq!(doc["effects"], "none");
    let record = &doc["ledger"][0];
    assert_eq!(record["tool"], "load_skill");
    assert_eq!(record["name"], "rust-review");
    assert_eq!(record["root"], "repo");
    assert_eq!(record["bytes"], 21);
    assert_eq!(record["sha256"].as_str().unwrap().len(), 64);
    assert!(record.get("text").is_none(), "{record}");
    let requests = gateway.join().unwrap();
    assert!(
        requests[0].contains("rust-review: Review Rust changes"),
        "the description is advertised: {}",
        requests[0]
    );
    assert!(
        requests[1].contains("Look twice."),
        "the text reaches the model as a tool result: {}",
        requests[1]
    );
    let audit = fs::read_to_string(dir.join("state/audit.log")).unwrap();
    let line: serde_json::Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(line["tool"], "load_skill");
    assert_eq!(line["name"], "rust-review");
    assert_eq!(line["root"], "repo");
    assert_eq!(line["run_id"], "skill-1");
    assert!(line.get("text").is_none());
    let rows = sessions(&dir);
    let id = rows[0]["id"].as_str().unwrap();
    let shown = ahi(&dir).args(["session", "show", id]).output().unwrap();
    assert_eq!(code(&shown), 0, "{}", err(&shown));
    assert!(
        out(&shown).contains("— skill rust-review (repo, 21 bytes)"),
        "{}",
        out(&shown)
    );
    assert!(
        !out(&shown).contains("Look twice"),
        "show never prints the text"
    );
    fs::remove_dir_all(dir).unwrap();
}
