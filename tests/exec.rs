//! `ahi exec` through the shipped binary: prompt sources, stdout and stderr
//! separation, the result file, and the exit codes, against a loopback
//! gateway. No real credentials, no external network.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
    assert_eq!(result["history"], "none");
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
