use super::*;
use crate::chat::backend::parse_selection;
use crate::chat::thinking::ThinkingLevel;
use crate::chat::transcript::ChatMessage;
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::thread;

// A real HTTP boundary: no credentials, external services, or paid inference.
fn server(status: &str, content_type: &str, body: &str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let handle = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no model request received"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            request.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        request.push_str(std::str::from_utf8(&body).unwrap());
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (url, handle)
}

fn state_for(url: &str) -> PathBuf {
    let state = crate::fixture_directory::create_unique_fixture_directory("chat-http");
    fs::write(
        state.join("models.json"),
        json!({"providers": {"local": {
            "baseUrl": url, "api": "openai-completions", "apiKey": "gateway",
            "headers": {"X-Gateway": "gateway"}, "models": [{"id": "vendor/model"}]
        }}})
        .to_string(),
    )
    .unwrap();
    state
}

fn request() -> ChatRequest {
    ChatRequest {
        selection: parse_selection("local/vendor/model"),
        thinking: ThinkingLevel::Medium,
        messages: vec![
            ChatMessage {
                role: TranscriptRole::Operator,
                text: "first".into(),
            },
            ChatMessage {
                role: TranscriptRole::Assistant,
                text: "previous reply".into(),
            },
            ChatMessage {
                role: TranscriptRole::Notice,
                text: "not for the model".into(),
            },
            ChatMessage {
                role: TranscriptRole::Operator,
                text: "why?".into(),
            },
        ],
    }
}

fn collect(backend: &mut HttpBackend, request: ChatRequest) -> Vec<ResponseEvent> {
    let receiver = backend.submit(request);
    let mut events = Vec::new();
    loop {
        let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let done = matches!(event, ResponseEvent::Completed | ResponseEvent::Failed(_));
        events.push(event);
        if done {
            return events;
        }
    }
}

#[test]
fn gateway_streams_model_text_and_real_usage_with_conversation_history() {
    let body = concat!(
        ": keepalive\r\n\r\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":12}}}\n\n",
        "data: [DONE]\n\n"
    );
    let (url, server) = server("200 OK", "text/event-stream; charset=utf-8", body);
    let state = state_for(&url);
    let events = collect(&mut HttpBackend::new(state.clone()), request());
    assert_eq!(
        events,
        vec![
            ResponseEvent::TextDelta("Hello ".into()),
            ResponseEvent::TextDelta("世界".into()),
            ResponseEvent::Usage {
                input_tokens: 42,
                output_tokens: 7,
                cached_input_tokens: Some(12),
                cost_usd: None
            },
            ResponseEvent::Completed,
        ]
    );
    let received = server.join().unwrap();
    let (headers, body) = received.split_once("\r\n\r\n").unwrap();
    assert!(
        headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "{headers}"
    );
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer gateway\r\n"));
    assert!(headers
        .to_ascii_lowercase()
        .contains("x-gateway: gateway\r\n"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        json!({
            "model": "vendor/model", "stream": true, "stream_options": {"include_usage": true},
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "previous reply"},
                {"role": "user", "content": "why?"}
            ]
        })
    );
    fs::remove_dir_all(state).unwrap();
}

#[test]
fn failures_do_not_echo_upstream_secrets_or_claim_completion() {
    for (status, content_type, body, expected) in [
        (
            "401 Unauthorized",
            "application/json",
            "secret-token",
            "model request failed (HTTP 401); no automatic retry was attempted",
        ),
        (
            "302 Found",
            "text/plain",
            "secret-token",
            "model request failed (HTTP 302); no automatic retry was attempted",
        ),
        (
            "200 OK",
            "application/json",
            "secret-token",
            "model response must be text/event-stream",
        ),
        (
            "200 OK",
            "text/event-stream",
            "data: secret-token\n\n",
            "invalid model response event",
        ),
        (
            "200 OK",
            "text/event-stream",
            "data: {\"error\":\"secret-token\"}\n\n",
            "model returned a streaming error",
        ),
        (
            "200 OK",
            "text/event-stream",
            "data: [DONE]\n\n",
            "model stream completed without a finish reason",
        ),
        (
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "model response ended before [DONE]",
        ),
        (
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
            "model finished tool calls without any calls",
        ),
    ] {
        let (url, server) = server(status, content_type, body);
        let state = state_for(&url);
        let events = collect(&mut HttpBackend::new(state.clone()), request());
        assert_eq!(events.last(), Some(&ResponseEvent::Failed(expected.into())));
        server.join().unwrap();
        fs::remove_dir_all(state).unwrap();
    }
}

fn tool_stream(chunks: &[serde_json::Value], reason: &str) -> String {
    let mut body = String::new();
    for calls in chunks {
        body.push_str(&format!(
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": {"tool_calls": calls}}]})
        ));
    }
    body.push_str(&format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
    ));
    body
}

#[test]
fn fragmented_parallel_tool_calls_are_validated_but_never_executed() {
    let body = tool_stream(
        &[
            json!([
                {"index": 1, "id": "call_b", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":"}},
                {"index": 0, "id": "call_a", "type": "function", "function": {"name": "read_", "arguments": "{\"path\":\"src/"}}
            ]),
            json!([
                {"index": 0, "function": {"name": "file", "arguments": "main.rs\"}"}},
                {"index": 1, "function": {"arguments": "\"echo secret\"}"}}
            ]),
        ],
        "tool_calls",
    );
    let (url, server) = server("200 OK", "text/event-stream", &body);
    let state = state_for(&url);
    let events = collect(&mut HttpBackend::new(state.clone()), request());
    assert_eq!(events, vec![
        ResponseEvent::ToolCallsRequested(vec![
            ToolCall { id: "call_a".into(), name: "read_file".into(), arguments: serde_json::from_value(json!({"path": "src/main.rs"})).unwrap() },
            ToolCall { id: "call_b".into(), name: "bash".into(), arguments: serde_json::from_value(json!({"command": "echo secret"})).unwrap() }
        ]),
        ResponseEvent::Failed("tool execution is disabled until an authorization policy and sandbox are implemented".into())
    ]);
    let received = server.join().unwrap();
    let (_, body) = received.split_once("\r\n\r\n").unwrap();
    // No tools are advertised before a policy-controlled executor exists.
    assert!(serde_json::from_str::<serde_json::Value>(body)
        .unwrap()
        .get("tools")
        .is_none());
    fs::remove_dir_all(state).unwrap();
}

#[test]
fn malformed_or_ambiguous_tool_calls_never_reach_the_policy_boundary() {
    let valid = json!({"index": 0, "id": "call_a", "type": "function", "function": {"name": "read_file", "arguments": "{}"}});
    for (chunks, reason, expected) in [
        (
            vec![
                json!([{ "index": 0, "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]),
            ],
            "tool_calls",
            "model tool call has no id",
        ),
        (
            vec![
                json!([valid.clone(), {"index": 1, "id": "call_a", "type": "function", "function": {"name": "bash", "arguments": "{}"}}]),
            ],
            "tool_calls",
            "model tool call ids are not unique",
        ),
        (
            vec![
                json!([valid.clone()]),
                json!([{"index": 0, "id": "changed"}]),
            ],
            "tool_calls",
            "invalid or changing model tool call id",
        ),
        (
            vec![
                json!([{"index": 0, "id": "call_a", "type": "function", "function": {"name": "read_file", "arguments": "[\"private\"]"}}]),
            ],
            "tool_calls",
            "model tool arguments must be a JSON object",
        ),
        (
            vec![json!([{"index": 32}])],
            "tool_calls",
            "model tool call index exceeds limit",
        ),
        (
            vec![
                json!([{"index": 0, "id": "call_a", "type": "function", "function": {"name": "read_file", "arguments": "x".repeat(65537)}}]),
            ],
            "tool_calls",
            "model tool arguments exceed limit",
        ),
        (
            vec![json!([valid])],
            "stop",
            "model tool calls require the tool_calls finish reason",
        ),
    ] {
        let body = tool_stream(&chunks, reason);
        let (sender, receiver) = mpsc::channel();
        let result = read_stream(body.as_bytes(), &sender);
        assert_eq!(result.err().unwrap().to_string(), expected);
        assert_eq!(receiver.try_iter().collect::<Vec<_>>(), Vec::new());
    }
}

#[test]
fn unimplemented_openai_and_stale_selections_fail_before_network() {
    let state = state_for("http://127.0.0.1:1/v1");
    let mut backend = HttpBackend::new(state.clone());
    let mut input = request();
    input.selection = parse_selection("openai/gpt-4.1");
    assert_eq!(collect(&mut backend, input), vec![ResponseEvent::Failed("OpenAI browser-login inference is not implemented yet; select a configured OpenAI-compatible gateway".into())]);
    let mut input = request();
    input.selection.model = "removed".into();
    assert_eq!(
        collect(&mut backend, input),
        vec![ResponseEvent::Failed(
            "selected model is no longer curated in models.json; reopen /model".into()
        )]
    );
    fs::remove_dir_all(state).unwrap();
}
