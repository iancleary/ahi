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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
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
    });
    (url, handle)
}

/// A state directory whose models.json curates `local/vendor/model` at `url`.
fn state(url: &str, api_key: &str) -> PathBuf {
    let dir = fixture_directory::create_unique_fixture_directory("exec");
    fs::write(
        dir.join("models.json"),
        format!(
            r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"{api_key}","models":[{{"id":"vendor/model"}}]}}}}}}"#
        ),
    )
    .unwrap();
    dir
}

fn ahi(state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ahi"));
    command
        .args(["--state-dir", state.to_str().unwrap(), "exec"])
        .stdin(Stdio::null());
    command
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
            dir.to_str().unwrap(),
            "exec",
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
    fs::write(dir.join("models.json"), "{ not json").unwrap();
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
