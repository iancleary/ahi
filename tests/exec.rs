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
    assert!(err(&second).contains("already answers"));

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

    // connect bridges stdin and stdout to the socket: one request, one answer,
    // then a subscription whose replayed entries arrive as events.
    let id = rows[0]["id"].as_str().unwrap();
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

    // stop ends the server cleanly and removes the socket.
    let stop = ahi(&dir)
        .args(["serve", "stop", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&stop), 0, "{}", err(&stop));
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
