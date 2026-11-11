//! `ahi exec` through the shipped binary: prompt sources, stdout and stderr
//! separation, the result file, and the exit codes, against a loopback
//! gateway. No real credentials, no external network.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

const STREAM: &str = concat!(
    ": keepalive\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":12}}}\n\n",
    "data: [DONE]\n\n"
);

/// Serve one HTTP response on a fresh loopback port and hand back the
/// request that arrived, headers and body.
fn gateway(status: &str, content_type: &str, body: &str) -> (String, JoinHandle<String>) {
    let (url, handle) = gateway_sequence(vec![(status, content_type, body.to_string())]);
    (url, thread::spawn(move || handle.join().unwrap().remove(0)))
}

/// Serve one response per connection, in order, and hand back every request.
fn gateway_sequence(responses: Vec<(&str, &str, String)>) -> (String, JoinHandle<Vec<String>>) {
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

fn serve_one(listener: &TcpListener, response: &str) -> String {
    // A binary that exits before connecting must fail the test, not
    // hang it: poll the listener against a deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no model request reached the gateway"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{error}"),
        }
    };
    // Accepted sockets can inherit O_NONBLOCK on macOS.
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
    vec![
        ("200 OK", "text/event-stream", first),
        ("200 OK", "text/event-stream", second),
    ]
}

/// A fixture root: `state/` holds models.json curating `local/vendor/model`
/// at `url`; `work/` is an empty tool root beside it, outside ahi's own
/// directories (which tools may never read).
fn state(url: &str, api_key: &str) -> PathBuf {
    let dir = fixture_directory::create_unique_fixture_directory("exec");
    fs::create_dir(dir.join("state")).unwrap();
    fs::write(
        dir.join("state/models.json"),
        format!(
            r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"{api_key}","models":[{{"id":"vendor/model"}}]}}}}}}"#
        ),
    )
    .unwrap();
    // An empty tool root: the tests run from the repository, whose own
    // ahi.tools.yaml would otherwise govern an interactive run.
    fs::create_dir(dir.join("work")).unwrap();
    dir
}

fn ahi(state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ahi"));
    command
        .args(["--state-dir", state.join("state").to_str().unwrap(), "exec"])
        .args(["--workdir", state.join("work").to_str().unwrap()])
        .stdin(Stdio::null());
    command
}

/// A result-file path under the fixture, outside the tool root.
fn result_path(dir: &Path, name: &str) -> String {
    dir.join(name).to_str().unwrap().to_string()
}

fn session_lines(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn audit_lines(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("state/audit.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn result(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn request_body(request: &str) -> serde_json::Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap()
}

#[test]
fn inline_prompt_streams_the_answer_to_stdout_and_the_result_file() {
    let (url, server) = gateway("200 OK", "text/event-stream", STREAM);
    let dir = state(&url, "gateway");
    let result_path = dir.join("out/result.json");
    fs::create_dir(dir.join("out")).unwrap();
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "Say hello.",
        "--result-file",
        result_path.to_str().unwrap(),
        "--run-id",
        "task-1",
    ]));
    let request = server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "Hello 世界\n");
    assert_eq!(
        text(&output.stderr),
        "",
        "a clean run says nothing on stderr"
    );
    let body = request_body(&request);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "Say hello.");
    assert!(body.get("tools").is_none(), "no tool is advertised: {body}");
    assert!(
        body.get("reasoning_effort").is_none(),
        "thinking off sends no reasoning field: {body}"
    );
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer gateway\r\n"));
    let result = result(&result_path);
    assert_eq!(result["schema"], 1);
    assert_eq!(result["ahi_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(result["run_id"], "task-1");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["gateway"], "local");
    assert_eq!(result["model"], "vendor/model");
    assert_eq!(result["text"], "Hello 世界");
    assert_eq!(result["text_truncated"], false);
    assert_eq!(result["usage"]["input_tokens"], 42);
    assert_eq!(result["usage"]["output_tokens"], 7);
    assert_eq!(result["usage"]["cached_input_tokens"], 12);
    assert_eq!(result["tools"]["calls"], 0);
    assert_eq!(result["effects"], "none");
    assert_eq!(result["ledger"], serde_json::json!([]));
    // Recorded by default: a new session under the state directory with the
    // prompt, the answer, and the turn line.
    assert_eq!(result["history"]["status"], "appended");
    assert_eq!(result["history"]["store"], "jsonl");
    assert_eq!(result["history"]["entries"], 3);
    let session = result["history"]["session"].as_str().unwrap();
    assert_eq!(session.len(), 36);
    let location = PathBuf::from(result["history"]["location"].as_str().unwrap());
    assert!(
        location.starts_with(dir.join("state/sessions")),
        "{location:?}"
    );
    let lines = session_lines(&location);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[0]["type"], "session");
    assert_eq!(lines[0]["id"], session);
    assert_eq!(
        lines[0]["cwd"],
        dir.join("work").canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(lines[1]["type"], "message");
    assert_eq!(lines[1]["role"], "operator");
    assert_eq!(lines[1]["text"], "Say hello.");
    assert_eq!(lines[2]["role"], "assistant");
    assert_eq!(lines[2]["text"], "Hello 世界");
    assert_eq!(lines[2]["parentId"], lines[1]["id"]);
    assert_eq!(lines[3]["type"], "turn");
    assert_eq!(lines[3]["run_id"], "task-1");
    assert_eq!(lines[3]["status"], "ok");
    assert_eq!(lines[3]["id"], result["history"]["tip"]);
    assert!(
        lines[3]["result"].get("text").is_none(),
        "the answer is not stored twice"
    );
    assert_eq!(lines[3]["result"]["effects"], "none");
    assert!(result.get("error").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&result_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn headless_takes_a_prompt_file_and_json_prints_the_result_object() {
    let (url, server) = gateway("200 OK", "text/event-stream", STREAM);
    let dir = state(&url, "gateway");
    fs::write(dir.join("task.md"), "Summarize the diff.\n").unwrap();
    let result_path = dir.join("result.json");
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--result-file",
        result_path.to_str().unwrap(),
        "--format",
        "json",
        "--thinking",
        "low",
    ]));
    let request = server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let body = request_body(&request);
    assert_eq!(body["messages"][0]["content"], "Summarize the diff.\n");
    // `--thinking low` is accepted; the field itself is sent only when the
    // model's metadata declares reasoning support, as in the chat.
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed, result(&result_path), "stdout and the file agree");
    assert_eq!(printed["status"], "ok");
    assert_eq!(
        printed["run_id"].as_str().unwrap().len(),
        16,
        "generated id"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stdin_is_the_prompt_when_nothing_else_names_one() {
    let (url, server) = gateway("200 OK", "text/event-stream", STREAM);
    let dir = state(&url, "gateway");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .args([
            "--state-dir",
            dir.join("state").to_str().unwrap(),
            "exec",
            "--workdir",
            dir.join("work").to_str().unwrap(),
            "--model",
            "local/vendor/model",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("piped prompt\n".as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let request = server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        request_body(&request)["messages"][0]["content"],
        "piped prompt\n"
    );
    assert_eq!(text(&output.stdout), "Hello 世界\n");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn usage_errors_exit_2_before_any_request() {
    // The port is bound and never accepts: a request would hang the test.
    let dir = state("http://127.0.0.1:1/v1", "gateway");
    let model = ["--model", "local/vendor/model"];
    let cases: Vec<(Vec<&str>, &str)> = vec![
        // clap: both prompt sources.
        (vec!["-p", "a", "--prompt-file", "b"], "cannot be used with"),
        // clap: headless without its files.
        (vec!["--headless", "-p", "a"], "required"),
        // Closed stdin and no prompt is an empty prompt.
        (vec![], "prompt is empty"),
        (vec!["-p", "  "], "prompt is empty"),
        (
            vec!["--prompt-file", "missing.md"],
            "cannot read prompt file",
        ),
        (vec!["-p", "a", "--run-id", "bad id"], "--run-id must be"),
    ];
    for (args, expected) in cases {
        let output = run(ahi(&dir).args(model).args(&args));
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    let output = run(ahi(&dir).args(["--model", "no-slash", "-p", "a"]));
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("gateway/model-id"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_existing_result_file_is_refused_and_left_alone() {
    let dir = state("http://127.0.0.1:1/v1", "gateway");
    let result_path = dir.join("result.json");
    fs::write(&result_path, "someone else's result\n").unwrap();
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--result-file",
        result_path.to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("already exists"));
    assert_eq!(
        fs::read_to_string(&result_path).unwrap(),
        "someone else's result\n"
    );
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--result-file",
        dir.join("nodir/result.json").to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("parent is not a directory"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn configuration_and_credential_faults_have_their_own_codes_and_a_result() {
    let dir = state("http://127.0.0.1:1/v1", "$AHI_EXEC_TEST_UNSET_CREDENTIAL");
    std::env::remove_var("AHI_EXEC_TEST_UNSET_CREDENTIAL");
    let result_path = dir.join("missing-credential.json");
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--result-file",
        result_path.to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(4), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("environment variable"));
    assert!(output.stdout.is_empty());
    let written = result(&result_path);
    assert_eq!(written["status"], "failed");
    assert_eq!(written["error"]["kind"], "auth");
    assert!(written.get("text").is_none());

    for (model, code, needle) in [
        ("nowhere/vendor/model", 3, "not registered"),
        ("local/other", 3, "no longer curated"),
    ] {
        let result_path = dir.join(format!("{code}-{}.json", model.replace('/', "_")));
        let output = run(ahi(&dir).args([
            "--model",
            model,
            "-p",
            "a",
            "--result-file",
            result_path.to_str().unwrap(),
        ]));
        assert_eq!(
            output.status.code(),
            Some(code),
            "{model}: {}",
            text(&output.stderr)
        );
        assert!(text(&output.stderr).contains(needle), "{model}");
        assert_eq!(result(&result_path)["error"]["kind"], "config");
    }
    fs::write(dir.join("state/models.json"), "{ not json").unwrap();
    let output = run(ahi(&dir).args(["--model", "local/vendor/model", "-p", "a"]));
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_failed_turn_leaves_stdout_empty_and_reports_the_kind() {
    let (url, server) = gateway(
        "500 Internal Server Error",
        "text/plain",
        "upstream secret body",
    );
    let dir = state(&url, "gateway");
    let result_path = dir.join("result.json");
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--result-file",
        result_path.to_str().unwrap(),
    ]));
    server.join().unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "nothing on stdout that reads as an answer"
    );
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(!stderr.contains("upstream secret body"), "{stderr}");
    let written = result(&result_path);
    assert_eq!(written["status"], "failed");
    assert_eq!(written["error"]["kind"], "transport");
    assert!(!written.to_string().contains("upstream secret body"));

    // A stream that ends without its terminal event is a protocol fault.
    let truncated = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n";
    let (url, server) = gateway("200 OK", "text/event-stream", truncated);
    let dir2 = state(&url, "gateway");
    let output = run(ahi(&dir2).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--format",
        "json",
    ]));
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert!(
        output.stdout.is_empty(),
        "partial text never reaches stdout"
    );
    fs::remove_dir_all(dir).unwrap();
    fs::remove_dir_all(dir2).unwrap();
}

#[test]
fn help_and_the_existing_surface_are_unchanged() {
    let output = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .args(["exec", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let help = text(&output.stdout);
    for flag in [
        "--model",
        "--prompt-file",
        "--result-file",
        "--headless",
        "--run-id",
        "--format",
        "--thinking",
        "--workdir",
        "--tools",
        "--check",
        "--prompt-sha256",
        "--verbose",
        "--timeout",
        "--history-file",
        "--no-history",
        "--history-model-change",
    ] {
        assert!(help.contains(flag), "{flag} missing from help");
    }
    assert!(help.contains("docs/HEADLESS_EXEC.md"));
    let top = Command::new(env!("CARGO_BIN_EXE_ahi"))
        .arg("--help")
        .output()
        .unwrap();
    let top = text(&top.stdout);
    for command in ["run", "tui", "exec", "tools", "login", "dev", "sandbox"] {
        assert!(
            top.contains(&format!("  {command}")),
            "{command} missing: {top}"
        );
    }
}

const WORKER_POLICY: &str = "version: 1\nrules:\n\
    - {id: reads, action: tool_call, match: {tool: read_file, path: src}, effect: approve}\n\
    - {id: edits, action: file_write, match: {tool: edit_file, path: src}, effect: approve}\n\
    - {id: shell, action: command, match: {argv: [sh]}, effect: approve, ask: true}\n\
    - {id: noop, action: command, match: {argv: ['true']}, effect: approve}\n\
    - {id: no-rm, action: command, match: {argv: [rm]}, effect: deny}\n";

/// A worker fixture: `work/src/a.rs` to edit, operator files beside it.
fn worker(url: &str) -> PathBuf {
    let dir = state(url, "gateway");
    fs::create_dir(dir.join("work/src")).unwrap();
    fs::write(dir.join("work/src/a.rs"), "fn a() {}\n").unwrap();
    fs::write(dir.join("task.md"), "Rename a to b.\n").unwrap();
    fs::write(dir.join("tools.yaml"), WORKER_POLICY).unwrap();
    dir
}

#[test]
fn headless_tools_run_under_the_policy_and_the_ledger_records_every_effect() {
    let (url, server) = gateway_sequence(tool_round(
        &[
            ("c1", "read_file", serde_json::json!({"path": "src/a.rs"})),
            (
                "c2",
                "edit_file",
                serde_json::json!({"path": "src/a.rs", "old": "fn a", "new": "fn b"}),
            ),
            (
                "c3",
                "run_command",
                serde_json::json!({"argv": ["sh", "-c", "echo secret-arg"]}),
            ),
            ("c4", "run_command", serde_json::json!({"argv": ["true"]})),
            (
                "c5",
                "run_command",
                serde_json::json!({"argv": ["rm", "-rf", "x"]}),
            ),
            (
                "c6",
                "edit_file",
                serde_json::json!({"path": "src/a.rs", "old": "absent", "new": "y"}),
            ),
        ],
        "done",
    ));
    let dir = worker(&url);
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "result.json"),
        "--run-id",
        "task-2",
    ]));
    let requests = server.join().unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&output.stdout), "done\n");

    // The first request advertised exactly the policy's tools; the second
    // carried one result per call, in order, with the unattended denial
    // explained to the model.
    let first = request_body(&requests[0]);
    let advertised: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(advertised, ["read_file", "edit_file", "run_command"]);
    let second = request_body(&requests[1]);
    let results: Vec<&serde_json::Value> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    assert_eq!(results.len(), 6);
    assert_eq!(results[0]["content"], "fn a() {}\n");
    assert!(results[2]["content"]
        .as_str()
        .unwrap()
        .contains("no operator is attending"));
    assert!(results[3]["content"]
        .as_str()
        .unwrap()
        .starts_with("exit 0"));
    assert!(results[4]["content"]
        .as_str()
        .unwrap()
        .contains("denied by rule no-rm"));
    assert!(results[5]["content"]
        .as_str()
        .unwrap()
        .starts_with("error:"));

    assert_eq!(
        fs::read_to_string(dir.join("work/src/a.rs")).unwrap(),
        "fn b() {}\n"
    );

    let result = result(&dir.join("result.json"));
    assert_eq!(result["status"], "ok");
    assert_eq!(result["tools"]["calls"], 6);
    assert_eq!(result["tools"]["rounds"], 1);
    assert_eq!(result["tools"]["denied"], 2, "the ask and the rm");
    assert_eq!(result["tools"]["ask_denied"], 1);
    assert_eq!(result["tools"]["invalid"], 1, "the failed edit");
    assert_eq!(result["effects"], "some");
    let ledger = result["ledger"].as_array().unwrap();
    assert_eq!(ledger.len(), 3, "{ledger:?}");
    assert_eq!(ledger[0]["tool"], "edit_file");
    assert_eq!(ledger[0]["target"], "src/a.rs");
    assert_eq!(ledger[0]["outcome"], "ok");
    assert_eq!(ledger[0]["lines_added"], 1);
    let before = ledger[0]["before"].as_str().unwrap();
    let after = ledger[0]["after"].as_str().unwrap();
    assert_eq!(before.len(), 64);
    assert_eq!(after.len(), 64);
    assert_ne!(before, after);
    assert_eq!(ledger[1]["tool"], "run_command");
    assert_eq!(ledger[1]["argv"], serde_json::json!(["true"]));
    assert_eq!(ledger[1]["cwd"], ".");
    assert_eq!(ledger[1]["exit_code"], 0);
    assert_eq!(ledger[1]["ran"], true);
    assert_eq!(ledger[2]["tool"], "edit_file");
    assert_ne!(ledger[2]["outcome"], "ok");
    assert!(ledger[2]["after"].is_null());
    let serialized = result.to_string();
    assert!(
        !serialized.contains("fn b"),
        "no file contents in the result"
    );
    assert!(
        !serialized.contains("secret-arg"),
        "denied argv is not recorded"
    );

    // Progress: one line per call, run id and round, tool and target, exit
    // code; never arguments, patterns, or output.
    let progress: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("progress:"))
        .collect();
    assert_eq!(progress.len(), 6, "{stderr}");
    assert!(progress[0].starts_with("progress: run task-2 round 1 read_file exit 0"));
    assert!(progress[1].starts_with("progress: run task-2 round 1 edit_file src/a.rs exit 0"));
    assert!(progress[2].contains("run_command exit 126"));
    assert!(
        !stderr.contains("secret-arg") && !stderr.contains("fn b"),
        "{stderr}"
    );

    // The audit log names the caller and the run, one line per effectful attempt.
    let audit = audit_lines(&dir);
    assert_eq!(audit.len(), 3, "{audit:?}");
    for line in &audit {
        assert_eq!(line["origin"], "exec");
        assert_eq!(line["run_id"], "task-2");
    }
    assert_eq!(audit[0]["tool"], "edit_file");
    assert_eq!(audit[0]["after"], ledger[0]["after"]);
    assert_eq!(audit[1]["argv"], serde_json::json!(["true"]));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn headless_without_tools_advertises_nothing_and_a_requested_call_fails_the_turn() {
    let (url, server) = gateway_sequence(vec![tool_round(
        &[("c1", "read_file", serde_json::json!({"path": "src/a.rs"}))],
        "unused",
    )
    .remove(0)]);
    let dir = worker(&url);
    // The work tree has no ahi.tools.yaml and none is named, so even an
    // approving file next door changes nothing.
    fs::write(dir.join("work/ahi.tools.yaml"), WORKER_POLICY).unwrap();
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "result.json"),
    ]));
    let requests = server.join().unwrap();
    assert!(request_body(&requests[0]).get("tools").is_none());
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert!(output.stdout.is_empty());
    let result = result(&dir.join("result.json"));
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error"]["kind"], "policy");
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("tool execution is disabled"));
    assert_eq!(result["effects"], "none");
    assert_eq!(result["tools"]["calls"], 1);
    assert_eq!(
        fs::read_to_string(dir.join("work/src/a.rs")).unwrap(),
        "fn a() {}\n"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn interactive_runs_read_the_workdir_policy_and_answer_asks_as_unattended_off_a_terminal() {
    let (url, server) = gateway_sequence(tool_round(
        &[
            ("c1", "read_file", serde_json::json!({"path": "src/a.rs"})),
            ("c2", "list_dir", serde_json::json!({"path": "src"})),
        ],
        "done",
    ));
    let dir = worker(&url);
    fs::write(
        dir.join("work/ahi.tools.yaml"),
        "version: 1\nrules:\n\
         - {id: ask-read, action: tool_call, match: {tool: read_file}, effect: approve, ask: true}\n\
         - {id: list, action: tool_call, match: {tool: list_dir}, effect: approve}\n",
    )
    .unwrap();
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "look around",
        "--verbose",
        "--format",
        "json",
    ]));
    let requests = server.join().unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["text"], "done");
    assert_eq!(printed["tools"]["ask_denied"], 1);
    assert_eq!(printed["tools"]["denied"], 1);
    assert_eq!(printed["effects"], "none");
    // The sibling the rule approved still ran: the batch was answered, not
    // cancelled.
    let second = request_body(&requests[1]);
    let results: Vec<&serde_json::Value> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    assert!(results[0]["content"]
        .as_str()
        .unwrap()
        .starts_with("denied:"));
    assert_eq!(results[1]["content"], "a.rs");
    assert_eq!(
        stderr
            .lines()
            .filter(|l| l.starts_with("progress:"))
            .count(),
        2,
        "{stderr}"
    );
    assert!(audit_lines(&dir).is_empty(), "reads are not audited");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn operator_files_must_lie_outside_a_working_directory_tools_can_read() {
    let dir = worker("http://127.0.0.1:1/v1");
    fs::write(dir.join("work/task.md"), "inside\n").unwrap();
    let base = |dir: &Path| {
        let mut command = ahi(dir);
        command.args(["--model", "local/vendor/model", "--headless"]);
        command
    };
    // Prompt inside.
    let output = run(base(&dir).args([
        "--prompt-file",
        dir.join("work/task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "r1.json"),
    ]));
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("prompt file"));
    assert!(text(&output.stderr).contains("inside the working directory"));
    // The refusal is reported where the coordinator will look for it.
    assert_eq!(result(&dir.join("r1.json"))["error"]["kind"], "usage");
    // Result inside.
    let output = run(base(&dir).args([
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        dir.join("work/src/result.json").to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("result file"));
    // Policy inside.
    fs::write(dir.join("work/tools.yaml"), WORKER_POLICY).unwrap();
    let output = run(base(&dir).args([
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("work/tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "r3.json"),
    ]));
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("policy file"));
    // Without a tool, the prompt may sit anywhere: nothing can read it.
    let output = run(base(&dir).args([
        "--prompt-file",
        dir.join("work/task.md").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "r4.json"),
        "--check",
    ]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_bad_policy_file_or_working_directory_stops_before_any_request() {
    let dir = worker("http://127.0.0.1:1/v1");
    fs::write(
        dir.join("broken.yaml"),
        "version: 1\nrules:\n  - {id: x, action: teleport}\n",
    )
    .unwrap();
    for (tools, needle) in [
        ("broken.yaml", "broken.yaml"),
        ("absent.yaml", "failed to read"),
    ] {
        let result_file = result_path(&dir, &format!("{tools}.json"));
        let output = run(ahi(&dir).args([
            "--model",
            "local/vendor/model",
            "--headless",
            "--prompt-file",
            dir.join("task.md").to_str().unwrap(),
            "--tools",
            dir.join(tools).to_str().unwrap(),
            "--result-file",
            &result_file,
        ]));
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(5), "{tools}: {stderr}");
        assert!(stderr.contains(needle), "{tools}: {stderr}");
        let written = result(Path::new(&result_file));
        assert_eq!(written["status"], "failed");
        assert_eq!(written["error"]["kind"], "policy");
    }
    // An invalid ahi.tools.yaml in the working directory is the same fault
    // for an interactive run.
    fs::write(dir.join("work/ahi.tools.yaml"), "version: 7\n").unwrap();
    let output = run(ahi(&dir).args(["--model", "local/vendor/model", "-p", "a"]));
    assert_eq!(output.status.code(), Some(5), "{}", text(&output.stderr));
    fs::remove_file(dir.join("work/ahi.tools.yaml")).unwrap();
    // A working directory that is not one.
    let output = run(Command::new(env!("CARGO_BIN_EXE_ahi"))
        .args(["--state-dir", dir.join("state").to_str().unwrap(), "exec"])
        .args(["--workdir", dir.join("task.md").to_str().unwrap()])
        .args(["--model", "local/vendor/model", "-p", "a"])
        .stdin(Stdio::null()));
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("not a directory"));
    let output = run(Command::new(env!("CARGO_BIN_EXE_ahi"))
        .args(["--state-dir", dir.join("state").to_str().unwrap(), "exec"])
        .args(["--workdir", dir.join("nowhere").to_str().unwrap()])
        .args(["--model", "local/vendor/model", "-p", "a"])
        .stdin(Stdio::null()));
    assert_eq!(output.status.code(), Some(2));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_validates_the_whole_contract_without_a_request_and_digests_guard_the_prompt() {
    let dir = worker("http://127.0.0.1:1/v1");
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "result.json"),
        "--check",
    ]));
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(stderr.starts_with("check: ok"), "{stderr}");
    assert!(
        stderr.contains("read_file,edit_file,run_command"),
        "{stderr}"
    );
    assert!(
        !dir.join("result.json").exists(),
        "a check writes no result"
    );

    // The digest of "Rename a to b.\n".
    let digest = {
        use sha2::Digest;
        sha2::Sha256::digest(b"Rename a to b.\n")
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>()
    };
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--prompt-sha256",
        &digest,
        "--check",
    ]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--prompt-sha256",
        &digest.replace('A', "B").replace('0', "1"),
        "--check",
    ]));
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("digest mismatch"));
    // A check still reports a missing credential as exit 4.
    let dir2 = state("http://127.0.0.1:1/v1", "$AHI_EXEC_CHECK_UNSET");
    std::env::remove_var("AHI_EXEC_CHECK_UNSET");
    let output = run(ahi(&dir2).args(["--model", "local/vendor/model", "-p", "a", "--check"]));
    assert_eq!(output.status.code(), Some(4), "{}", text(&output.stderr));
    fs::remove_dir_all(dir).unwrap();
    fs::remove_dir_all(dir2).unwrap();
}

/// A gateway that reads one request and then says nothing for `hold`. The
/// flag is set once the request has fully arrived.
fn silent_gateway(hold: Duration) -> (String, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let arrived = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&arrived);
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; length];
        let _ = reader.read_exact(&mut body);
        flag.store(true, Ordering::SeqCst);
        thread::sleep(hold);
        drop(stream);
    });
    (url, arrived)
}

#[test]
fn a_timeout_cancels_a_silent_turn_and_reports_it() {
    let (url, _arrived) = silent_gateway(Duration::from_secs(8));
    let dir = state(&url, "gateway");
    let started = Instant::now();
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--timeout",
        "1",
        "--result-file",
        &result_path(&dir, "result.json"),
    ]));
    let elapsed = started.elapsed();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(6), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("timed out after 1 s"), "{stderr}");
    assert!(elapsed < Duration::from_secs(7), "took {elapsed:?}");
    let result = result(&dir.join("result.json"));
    assert_eq!(result["status"], "timeout");
    assert_eq!(result["error"]["kind"], "timeout");
    assert_eq!(result["effects"], "none");
    assert!(result.get("text").is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_timeout_kills_a_running_command_and_the_ledger_says_so() {
    // Only the tool round is served: the turn never gets to ask for more.
    let (url, server) = gateway_sequence(vec![tool_round(
        &[(
            "c1",
            "run_command",
            serde_json::json!({"argv": ["sh", "-c", "sleep 30"], "timeout": 600}),
        )],
        "unused",
    )
    .remove(0)]);
    let dir = worker(&url);
    fs::write(
        dir.join("tools.yaml"),
        "version: 1\nrules:\n  - {id: shell, action: command, match: {argv: [sh]}, effect: approve}\n",
    )
    .unwrap();
    let started = Instant::now();
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "result.json"),
        "--timeout",
        "2",
    ]));
    server.join().unwrap();
    let elapsed = started.elapsed();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(6), "{stderr}");
    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    let result = result(&dir.join("result.json"));
    assert_eq!(result["status"], "timeout");
    // The command was killed and that is on the record: a program ran, so
    // effects are known, not unknown. Which guard fired first is a race
    // between the command's clamped timeout and the run's cancel; both say
    // the program ran and was killed.
    assert_eq!(result["effects"], "some");
    let ledger = result["ledger"].as_array().unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    let outcome = ledger[0]["outcome"].as_str().unwrap();
    assert!(
        outcome == "command cancelled and killed" || outcome == "command timed out and was killed",
        "{outcome}"
    );
    assert_eq!(ledger[0]["ran"], true);
    assert!(ledger[0]["exit_code"].is_null());
    assert!(stderr.contains("run_command exit 1"), "{stderr}");
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_signal_writes_the_result_as_cancelled_before_exiting() {
    let (url, arrived) = silent_gateway(Duration::from_secs(8));
    let dir = state(&url, "gateway");
    let child = ahi(&dir)
        .args([
            "--model",
            "local/vendor/model",
            "-p",
            "a",
            "--timeout",
            "60",
            "--result-file",
            &result_path(&dir, "result.json"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !arrived.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the request never arrived");
        thread::sleep(Duration::from_millis(10));
    }
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(6), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("cancelled by SIGTERM"), "{stderr}");
    let result = result(&dir.join("result.json"));
    assert_eq!(result["status"], "cancelled");
    assert_eq!(result["error"]["kind"], "cancelled");
    assert_eq!(result["error"]["message"], "cancelled by SIGTERM");
    fs::remove_dir_all(dir).unwrap();
}

/// One answering gateway and a run against it with `extra` arguments.
fn answer(dir: &Path, text: &str, extra: &[&str]) -> (Output, String) {
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}]})
    );
    let (url, server) = gateway("200 OK", "text/event-stream", &body);
    fs::write(
        dir.join("state/models.json"),
        format!(
            r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"gateway","models":[{{"id":"vendor/model"}},{{"id":"vendor/other"}}]}}}}}}"#
        ),
    )
    .unwrap();
    let output = run(ahi(dir).args(extra));
    (output, server.join().unwrap())
}

#[test]
fn no_history_and_check_record_nothing() {
    let (url, server) = gateway("200 OK", "text/event-stream", STREAM);
    let dir = state(&url, "gateway");
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "-p",
        "a",
        "--no-history",
        "--format",
        "json",
    ]));
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["history"], "none");
    assert!(!dir.join("state/sessions").exists());
    let output = run(ahi(&dir).args(["--model", "local/vendor/model", "-p", "a", "--check"]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        !dir.join("state/sessions").exists(),
        "a check creates no session"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_history_file_carries_the_conversation_and_a_failed_turn_stays_a_branch() {
    let dir = state("http://127.0.0.1:1/v1", "gateway");
    let history = dir.join("session.jsonl");
    let file = ["--history-file", history.to_str().unwrap()];
    let model = ["--model", "local/vendor/model"];
    let (first, request) = answer(&dir, "a1", &[&model[..], &["-p", "q1"], &file[..]].concat());
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert_eq!(
        request_body(&request)["messages"].as_array().unwrap().len(),
        1
    );
    assert_eq!(text(&first.stdout), "a1\n");

    // The second turn fails at the gateway: it is recorded, and abandoned.
    let (url, server) = gateway("500 Internal Server Error", "text/plain", "down");
    fs::write(
        dir.join("state/models.json"),
        format!(
            r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"gateway","models":[{{"id":"vendor/model"}}]}}}}}}"#
        ),
    )
    .unwrap();
    let second = run(ahi(&dir).args(model).args(["-p", "q2"]).args(file));
    server.join().unwrap();
    assert_eq!(second.status.code(), Some(1), "{}", text(&second.stderr));

    // The third sees q1 and a1 only.
    let (third, request) = answer(
        &dir,
        "a3",
        &[&model[..], &["-p", "q3", "--format", "json"], &file[..]].concat(),
    );
    assert_eq!(third.status.code(), Some(0), "{}", text(&third.stderr));
    let messages = request_body(&request)["messages"].clone();
    let texts: Vec<&str> = messages
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["q1", "a1", "q3"]);
    let printed: serde_json::Value = serde_json::from_slice(&third.stdout).unwrap();
    assert_eq!(printed["history"]["status"], "appended");
    assert_eq!(printed["history"]["location"], history.to_str().unwrap());

    let lines = session_lines(&history);
    let types: Vec<String> = lines
        .iter()
        .map(|line| {
            format!(
                "{}{}",
                line["type"].as_str().unwrap(),
                line["status"]
                    .as_str()
                    .map(|s| format!(":{s}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        types,
        [
            "session",
            "message",
            "message",
            "turn:ok",
            "message",
            "turn:failed",
            "message",
            "message",
            "turn:ok"
        ]
    );
    // Both later turns hang off the first success; the failed branch is
    // not a parent of anything.
    assert_eq!(lines[4]["parentId"], lines[3]["id"]);
    assert_eq!(lines[6]["parentId"], lines[3]["id"]);
    assert_eq!(lines[5]["result"]["error"]["kind"], "transport");
    assert_eq!(lines[0]["id"], printed["history"]["session"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_resubmitted_run_id_is_answered_from_the_record_without_a_request() {
    let dir = state("http://127.0.0.1:1/v1", "gateway");
    let history = dir.join("session.jsonl");
    let base = |dir: &Path| {
        let mut command = ahi(dir);
        command.args([
            "--model",
            "local/vendor/model",
            "--history-file",
            history.to_str().unwrap(),
        ]);
        command
    };
    let (first, _) = answer(
        &dir,
        "forty-two",
        &[
            "--model",
            "local/vendor/model",
            "--history-file",
            history.to_str().unwrap(),
            "-p",
            "the answer?",
            "--run-id",
            "t1",
        ],
    );
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    // The gateway is now unreachable: a replay must not need it.
    fs::write(
        dir.join("state/models.json"),
        r#"{"providers":{"local":{"baseUrl":"http://127.0.0.1:1/v1","api":"openai-completions","apiKey":"gateway","models":[{"id":"vendor/model"}]}}}"#,
    )
    .unwrap();
    let again = run(base(&dir).args([
        "-p",
        "the answer?",
        "--run-id",
        "t1",
        "--result-file",
        &result_path(&dir, "replay.json"),
    ]));
    let stderr = text(&again.stderr);
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&again.stdout), "forty-two\n");
    assert!(stderr.contains("already recorded"), "{stderr}");
    let replayed = result(&dir.join("replay.json"));
    assert_eq!(replayed["status"], "ok");
    assert_eq!(replayed["text"], "forty-two");
    assert_eq!(replayed["run_id"], "t1");
    assert_eq!(replayed["history"]["status"], "replayed");
    assert_eq!(session_lines(&history).len(), 4, "a replay writes nothing");

    let other = run(base(&dir).args(["-p", "a different task", "--run-id", "t1"]));
    assert_eq!(other.status.code(), Some(7), "{}", text(&other.stderr));
    assert!(text(&other.stderr).contains("different prompt"));
    assert_eq!(session_lines(&history).len(), 4);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_session_recorded_under_another_model_needs_the_flag_once() {
    let dir = state("http://127.0.0.1:1/v1", "gateway");
    let history = dir.join("session.jsonl");
    let file = ["--history-file", history.to_str().unwrap()];
    let (first, _) = answer(
        &dir,
        "a1",
        &[
            &["--model", "local/vendor/model", "-p", "q1"][..],
            &file[..],
        ]
        .concat(),
    );
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));

    let refused = run(ahi(&dir)
        .args(["--model", "local/vendor/other", "-p", "q2"])
        .args(file));
    assert_eq!(refused.status.code(), Some(7), "{}", text(&refused.stderr));
    assert!(text(&refused.stderr).contains("--history-model-change"));
    assert_eq!(session_lines(&history).len(), 4, "nothing written");

    let (second, request) = answer(
        &dir,
        "a2",
        &[
            &[
                "--model",
                "local/vendor/other",
                "-p",
                "q2",
                "--history-model-change",
            ][..],
            &file[..],
        ]
        .concat(),
    );
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    assert_eq!(request_body(&request)["model"], "vendor/other");
    let lines = session_lines(&history);
    assert_eq!(lines[4]["type"], "model_change");
    assert_eq!(lines[4]["model"], "vendor/other");
    assert_eq!(lines[5]["role"], "operator");

    // Already changed on the active path: no flag needed the next time.
    let (third, request) = answer(
        &dir,
        "a3",
        &[
            &["--model", "local/vendor/other", "-p", "q3"][..],
            &file[..],
        ]
        .concat(),
    );
    assert_eq!(third.status.code(), Some(0), "{}", text(&third.stderr));
    assert_eq!(
        request_body(&request)["messages"].as_array().unwrap().len(),
        5
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tool_rounds_are_recorded_call_first_then_results() {
    let (url, server) = gateway_sequence(tool_round(
        &[
            ("c1", "read_file", serde_json::json!({"path": "src/a.rs"})),
            (
                "c2",
                "run_command",
                serde_json::json!({"argv": ["rm", "x"]}),
            ),
        ],
        "done",
    ));
    let dir = worker(&url);
    let history = dir.join("session.jsonl");
    let output = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "result.json"),
        "--history-file",
        history.to_str().unwrap(),
    ]));
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let lines = session_lines(&history);
    let roles: Vec<String> = lines[1..]
        .iter()
        .map(|line| {
            line["role"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| line["type"].as_str().unwrap().to_string())
        })
        .collect();
    assert_eq!(
        roles,
        ["operator", "assistant", "tool", "tool", "assistant", "turn"]
    );
    assert_eq!(lines[2]["calls"][0]["name"], "read_file");
    assert_eq!(
        lines[2]["calls"][1]["arguments"]["argv"],
        serde_json::json!(["rm", "x"])
    );
    assert_eq!(lines[3]["exit_code"], 0);
    assert_eq!(lines[3]["result"]["call_id"], "c1");
    assert_eq!(lines[3]["result"]["content"], "fn a() {}\n");
    assert_eq!(lines[4]["exit_code"], 126);
    assert!(lines[4]["result"]["content"]
        .as_str()
        .unwrap()
        .starts_with("denied: denied by rule no-rm"));
    assert_eq!(lines[5]["text"], "done");
    assert_eq!(lines[6]["result"]["ledger"], serde_json::json!([]));
    assert_eq!(result(&dir.join("result.json"))["history"]["entries"], 6);

    // The history file counts as an operator file.
    let inside = run(ahi(&dir).args([
        "--model",
        "local/vendor/model",
        "--headless",
        "--prompt-file",
        dir.join("task.md").to_str().unwrap(),
        "--tools",
        dir.join("tools.yaml").to_str().unwrap(),
        "--result-file",
        &result_path(&dir, "r2.json"),
        "--history-file",
        dir.join("work/session.jsonl").to_str().unwrap(),
    ]));
    assert_eq!(inside.status.code(), Some(2));
    assert!(text(&inside.stderr).contains("history file"));
    fs::remove_dir_all(dir).unwrap();
}
