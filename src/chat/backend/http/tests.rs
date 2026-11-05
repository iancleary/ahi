use super::*;
use crate::chat::backend::parse_selection;
use crate::chat::backend::{ProposedAction, ToolDecision};
use crate::chat::thinking::ThinkingLevel;
use crate::chat::transcript::ChatMessage;
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::thread;

const RESPONSE_SECRET_MARKER: &str = "synthetic-response-secret-marker";

// A real HTTP boundary: no credentials, external services, or paid inference.
fn server(
    status: &str,
    content_type: Option<&str>,
    body: &str,
) -> (String, thread::JoinHandle<String>) {
    let (url, handle) = server_sequence(vec![response(status, content_type, body)]);
    (url, thread::spawn(move || handle.join().unwrap().remove(0)))
}

fn response(status: &str, content_type: Option<&str>, body: &str) -> String {
    let content_type = content_type
        .map(|value| format!("Content-Type: {value}\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn stream(body: &str) -> String {
    response("200 OK", Some("text/event-stream"), body)
}

/// Serve one response per connection, in order, and return each request.
fn server_sequence(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| serve_one(&listener, &response))
            .collect()
    });
    (url, handle)
}

fn serve_one(listener: &TcpListener, response: &str) -> String {
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
    // Accepted sockets can inherit O_NONBLOCK on macOS. The listener polls,
    // but each connected fixture request must use blocking, timed reads.
    stream.set_nonblocking(false).unwrap();
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
    for fragment in response.as_bytes().chunks(7) {
        if stream.write_all(fragment).is_err() {
            break;
        }
    }
    request
}

fn request_body(request: &str) -> serde_json::Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn state_for(url: &str) -> PathBuf {
    let state = crate::fixture_directory::create_unique_fixture_directory("chat-http");
    write_models(&state, url, json!({"id": "vendor/model"}));
    state
}

fn write_models(state: &Path, url: &str, model: serde_json::Value) {
    fs::write(
        state.join("models.json"),
        json!({"providers": {"local": {
            "baseUrl": url, "api": "openai-completions", "apiKey": "gateway",
            "headers": {"X-Gateway": "gateway"}, "models": [model]
        }}})
        .to_string(),
    )
    .unwrap();
}

/// Never the process working directory: a repository `ahi.actions.yaml`
/// must not change these tests. The empty workspace enables no tools.
fn backend(state: &Path) -> HttpBackend {
    let workspace = state.join("empty-workspace");
    fs::create_dir_all(&workspace).unwrap();
    HttpBackend::with_workspace(state.to_path_buf(), workspace)
}

fn responses_fixture(body: &str) -> (Result<()>, Vec<ResponseEvent>, String) {
    responses_fixture_with_refresh(body, false, Some("text/event-stream; charset=utf-8"))
}

fn responses_fixture_with_refresh(
    body: &str,
    refresh: bool,
    content_type: Option<&str>,
) -> (Result<()>, Vec<ResponseEvent>, String) {
    let token_server = refresh.then(|| server("200 OK", Some("application/json"), r#"{"access_token":"disposable-browser-token","refresh_token":"rotated-disposable-token","expires_in":3600,"scope":"chatgpt.tokens.use.direct"}"#));
    let token_url = token_server
        .as_ref()
        .map(|(url, _)| format!("{url}/token"))
        .unwrap_or_else(|| "http://127.0.0.1:1/token".into());
    let (url, server) = server("200 OK", content_type, body);
    let state = state_for(&url);
    fs::create_dir(state.join("credentials")).unwrap();
    fs::write(state.join("credentials/openai.json"), json!({
        "access_token":"disposable-browser-token", "refresh_token":"disposable-refresh-token",
        "expires_in":3600, "expires_at":if refresh {0} else {i64::MAX}, "client_id":"fixture-client",
        "resource":"https://api.openai.com/v1", "scopes":["chatgpt.tokens.use.direct"], "flow":"chatgpt-direct"
    }).to_string()).unwrap();
    let mut input = request();
    input.selection = parse_selection("openai/curated-model");
    let metadata = crate::harness::ModelMetadata::default();
    let body_bytes = responses::request_body(&input, &metadata).unwrap();
    let response = crate::harness::fixture_inference_response(
        &state,
        &body_bytes,
        &format!("{url}/responses"),
        &token_url,
    )
    .unwrap();
    let (sender, receiver) = mpsc::channel();
    let result = responses::consume(response, &sender, &metadata);
    let events = receiver.try_iter().collect();
    let received = server.join().unwrap();
    if let Some((_, token_server)) = token_server {
        let received = token_server.join().unwrap();
        assert!(received.starts_with("POST /v1/token HTTP/1.1"));
        assert!(received.contains("refresh_token=disposable-refresh-token"));
        assert!(received.contains("client_id=fixture-client"));
        let credential: serde_json::Value =
            serde_json::from_slice(&fs::read(state.join("credentials/openai.json")).unwrap())
                .unwrap();
        assert_eq!(credential["refresh_token"], "rotated-disposable-token");
    }
    fs::remove_dir_all(state).unwrap();
    (result, events, received)
}

fn response_events(events: Vec<serde_json::Value>) -> String {
    events
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\r\ndata: {event}\r\n\r\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}

fn response_message(text: &str) -> serde_json::Value {
    json!({"id":"msg_fixture", "type":"message", "role":"assistant", "status":"completed", "content":[{"type":"output_text", "text":text,"annotations":[]}]})
}

fn completed_response(output: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"type":"response.completed", "response":{"id":"resp_fixture", "status":"completed", "output":output, "usage":{"input_tokens":42,"output_tokens":7,"input_tokens_details":{"cached_tokens":12}}}})
}

fn verify_responses_http_workflow() {
    let final_item = response_message("Hello 世界");
    let mut pending = final_item.clone();
    pending["content"] = json!([]);
    pending["status"] = json!("in_progress");
    let body = response_events(vec![
        json!({"type":"response.created", "response":{"id":"resp_fixture"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":pending}),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"item_id":"msg_fixture","delta":"Hello "}),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"item_id":"msg_fixture","delta":"世界"}),
        json!({"type":"response.output_item.done","output_index":0,"item":final_item}),
        completed_response(vec![final_item]),
    ]);
    let body = format!(": {RESPONSE_SECRET_MARKER}\r\n\r\n{body}");
    let (result, events, received) =
        responses_fixture_with_refresh(&body, true, Some("text/event-stream; charset=utf-8"));
    result.unwrap();
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
            }
        ]
    );
    let (result, mislabeled_events, _) =
        responses_fixture_with_refresh(&body, false, Some("application/json"));
    result.expect("valid completed SSE must be accepted despite application/json Content-Type");
    assert_eq!(mislabeled_events, events);
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/x-synthetic-response-secret-marker"),
    ] {
        let (result, alternate_events, _) =
            responses_fixture_with_refresh(&body, false, content_type);
        result.expect(
            "valid completed SSE must be accepted despite missing or incorrect Content-Type",
        );
        assert_eq!(alternate_events, events);
        assert!(!format!("{alternate_events:?}").contains(RESPONSE_SECRET_MARKER));
    }
    let (headers, body) = received.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer disposable-browser-token"));
    assert!(!headers.to_ascii_lowercase().contains("chatgpt-account-id"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        json!({
            "model":"curated-model", "stream":true,"store":false,
            "input":[{"role":"user","content":[{"type":"input_text","text":"first"}]},{"role":"assistant","content":"previous reply"},{"role":"user","content":[{"type":"input_text","text":"why?"}]}]
        })
    );
    let metadata = serde_json::from_value::<crate::harness::ModelMetadata>(
        json!({"reasoning":true,"maxTokens":20,"compat":{"supportsReasoningEffort":true}}),
    )
    .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&responses::request_body(&request(), &metadata).unwrap()).unwrap();
    assert_eq!(body["reasoning"], json!({"effort":"medium"}));
    assert!(body.get("max_output_tokens").is_none());
    let metadata = serde_json::from_value::<crate::harness::ModelMetadata>(
        json!({"inputLimits":{"maxRequestBytes":1}}),
    )
    .unwrap();
    assert!(responses::request_body(&request(), &metadata).is_err());
}

fn verify_responses_http_failures() {
    let item = response_message("partial");
    let added = json!({"type":"response.output_item.added","output_index":0,"item":item});
    let delta = json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"item_id":"msg_fixture","delta":"partial"});
    for (body, expected) in [
        (
            "data: private upstream text\n\n".to_owned(),
            "invalid or conflicting",
        ),
        (
            response_events(vec![added.clone(), delta.clone()]),
            "ended before a terminal",
        ),
        (
            response_events(vec![
                json!({"type":"error","message":"private upstream text"}),
            ]),
            "streaming failure",
        ),
        (
            response_events(vec![
                json!({"type":"response.failed","response":{"error":{"message":"private upstream text"}}}),
            ]),
            "streaming failure",
        ),
        (
            response_events(vec![
                json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
            ]),
            "truncated",
        ),
        (
            response_events(vec![
                json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"content_filter"}}}),
            ]),
            "incomplete",
        ),
        (
            response_events(vec![
                json!({"type":"response.refusal.delta","delta":"private upstream text"}),
            ]),
            "refused",
        ),
        (
            response_events(vec![
                added.clone(),
                delta,
                completed_response(vec![item.clone()]),
            ]),
            "unfinished",
        ),
        (
            response_events(vec![added.clone(), added]),
            "invalid or conflicting",
        ),
        (
            response_events(vec![
                json!({"type":"response.completed","response":{"id":"resp_fixture","status":"in_progress","output":[]}}),
            ]),
            "invalid or conflicting",
        ),
        (
            response_events(vec![
                json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":[],"error":{"message":"private upstream text"}}}),
            ]),
            "invalid or conflicting",
        ),
        (
            response_events(vec![
                json!({"type":"response.output_item.added","output_index":0,"item":response_message("")}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg_fixture","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"private upstream text"}]}}),
            ]),
            "refused",
        ),
        (
            format!("data: {}\n\n", "x".repeat(FRAME_CAP as usize + 1)),
            "size limit",
        ),
    ] {
        let body = format!(": {RESPONSE_SECRET_MARKER}\n\n{body}");
        let (result, events, _) = responses_fixture(&body);
        let diagnostic = result.unwrap_err().to_string();
        assert!(diagnostic.contains(expected), "{diagnostic}");
        assert!(diagnostic.contains("HTTP 200; Content-Type: text/event-stream"));
        assert!(!diagnostic.contains("private upstream text"));
        assert!(!diagnostic.contains(RESPONSE_SECRET_MARKER));
        assert!(!format!("{events:?}").contains(RESPONSE_SECRET_MARKER));
        assert!(!events.iter().any(|event| matches!(
            event,
            ResponseEvent::Completed | ResponseEvent::ToolCallsRequested(_)
        )));
    }
    let bodies = [
        (
            json!({"error":{"message":RESPONSE_SECRET_MARKER,"code":RESPONSE_SECRET_MARKER,"param":RESPONSE_SECRET_MARKER}}).to_string(),
            "non-SSE JSON-like body",
        ),
        (
            serde_json::to_string_pretty(&completed_response(vec![response_message(RESPONSE_SECRET_MARKER)])["response"]).unwrap(),
            "non-SSE JSON-like body",
        ),
        (
            format!(" \r\n<!DOCTYPE html><html><body>Access denied: {RESPONSE_SECRET_MARKER}</body></html>"),
            "non-SSE HTML/markup body",
        ),
        (
            format!("data: {RESPONSE_SECRET_MARKER}\n\n"),
            "invalid or conflicting",
        ),
        (
            response_events(vec![json!({"type":"response.created","response":{"id":"resp_fixture"},"private":RESPONSE_SECRET_MARKER})]),
            "ended before a terminal",
        ),
        (
            format!("data: {{\"type\":\"response.completed\",\"private\":\"{RESPONSE_SECRET_MARKER}\"}}"),
            "ended before a terminal",
        ),
    ];
    for (content_type, category) in [
        (Some("text/event-stream"), "text/event-stream"),
        (Some("APPLICATION/JSON; charset=utf-8"), "application/json"),
        (Some("text/html"), "text/html"),
        (Some("text/plain"), "text/plain"),
        (Some("application/octet-stream"), "application/octet-stream"),
        (None, "missing"),
        (
            Some("application/x-synthetic-response-secret-marker; token=synthetic-header-secret"),
            "other",
        ),
        (
            Some("text/event-stream; token=synthetic-response-secret-marker"),
            "text/event-stream",
        ),
    ] {
        for (body, expected) in &bodies {
            let (result, events, _) = responses_fixture_with_refresh(body, false, content_type);
            let diagnostic = result.unwrap_err().to_string();
            assert!(diagnostic.contains(expected), "{diagnostic}");
            assert!(
                diagnostic.contains(&format!("HTTP 200; Content-Type: {category})")),
                "{diagnostic}"
            );
            assert!(!diagnostic.contains(RESPONSE_SECRET_MARKER));
            assert!(!diagnostic.contains("synthetic-header-secret"));
            assert!(events.is_empty(), "{events:?}");
            assert!(!format!("{events:?}").contains(RESPONSE_SECRET_MARKER));
        }
    }
}

fn verify_responses_tool_requests() {
    let first = json!({"type":"function_call","id":"fc_first","call_id":"call_first","name":"read_file","arguments":"{\"path\":\"src/世界.rs\"}"});
    let second = json!({"type":"function_call","id":"fc_second","call_id":"call_second","name":"read_file","arguments":"{}"});
    let mut pending = first.clone();
    pending["arguments"] = json!("");
    let body = response_events(vec![
        json!({"type":"response.output_item.added","output_index":0,"item":pending}),
        json!({"type":"response.output_item.added","output_index":1,"item":second}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc_first","delta":"{\"path\":"}),
        json!({"type":"response.output_item.done","output_index":1,"item":second}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc_first","delta":"\"src/世界.rs\"}"}),
        json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"fc_first","arguments":first["arguments"]}),
        json!({"type":"response.output_item.done","output_index":0,"item":first}),
        completed_response(vec![first.clone(), second.clone()]),
    ]);
    let (result, events, _) = responses_fixture(&body);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("tool execution is disabled"));
    assert!(
        matches!(&events[..], [ResponseEvent::Usage {..}, ResponseEvent::ToolCallsRequested(calls)] if calls.len() == 2 && calls[0].id == "call_first" && calls[0].arguments["path"] == "src/世界.rs")
    );
    for (first, second, complete) in [
        (first.clone(), second.clone(), false),
        (
            {
                let mut value = first.clone();
                value["status"] = json!("in_progress");
                value
            },
            second.clone(),
            true,
        ),
        (
            {
                let mut value = first.clone();
                value["arguments"] = json!("[\"private\"]");
                value
            },
            second.clone(),
            true,
        ),
        (
            first.clone(),
            {
                let mut value = second.clone();
                value["call_id"] = json!("call_first");
                value
            },
            true,
        ),
        (
            first.clone(),
            {
                let mut value = second;
                value["name"] = json!("invalid name");
                value
            },
            true,
        ),
    ] {
        let mut events = vec![
            json!({"type":"response.output_item.added","output_index":0,"item":first}),
            json!({"type":"response.output_item.added","output_index":1,"item":second}),
        ];
        if complete {
            events.push(json!({"type":"response.output_item.done","output_index":0,"item":first}));
            events.push(json!({"type":"response.output_item.done","output_index":1,"item":second}));
        }
        events.push(completed_response(vec![first, second]));
        let (result, events, _) = responses_fixture(&response_events(events));
        assert!(result.is_err());
        assert!(!events
            .iter()
            .any(|event| matches!(event, ResponseEvent::ToolCallsRequested(_))));
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        selection: parse_selection("local/vendor/model"),
        thinking: ThinkingLevel::Medium,
        messages: vec![
            ChatMessage {
                role: TranscriptRole::Operator,
                text: "first".into(),
                tool: None,
            },
            ChatMessage {
                role: TranscriptRole::Assistant,
                text: "previous reply".into(),
                tool: None,
            },
            ChatMessage {
                role: TranscriptRole::Notice,
                text: "not for the model".into(),
                tool: None,
            },
            ChatMessage {
                role: TranscriptRole::Operator,
                text: "why?".into(),
                tool: None,
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
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(5)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ));
            return events;
        }
    }
}

#[test]
fn gateway_streams_model_text_and_real_usage_with_conversation_history() {
    verify_responses_http_workflow();
    let body = concat!(
        ": keepalive\r\n\r\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":12}}}\n\n",
        "data: [DONE]\n\n"
    );
    let (url, server) = server("200 OK", Some("text/event-stream; charset=utf-8"), body);
    let state = state_for(&url);
    let events = collect(&mut backend(&state), request());
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
    verify_responses_http_failures();
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
        let (url, server) = server(status, Some(content_type), body);
        let state = state_for(&url);
        let events = collect(&mut backend(&state), request());
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
    verify_responses_tool_requests();
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
    let (url, server) = server("200 OK", Some("text/event-stream"), &body);
    let state = state_for(&url);
    let events = collect(&mut backend(&state), request());
    assert_eq!(events, vec![
        ResponseEvent::ToolCallsRequested(vec![
            ToolCall { id: "call_a".into(), name: "read_file".into(), arguments: serde_json::from_value(json!({"path": "src/main.rs"})).unwrap() },
            ToolCall { id: "call_b".into(), name: "bash".into(), arguments: serde_json::from_value(json!({"command": "echo secret"})).unwrap() }
        ]),
        ResponseEvent::Failed("tool execution is disabled: no ahi.actions.yaml rule approves a builtin tool in this working directory".into())
    ]);
    let received = server.join().unwrap();
    let (_, body) = received.split_once("\r\n\r\n").unwrap();
    // Without an approving ahi.actions.yaml rule, no tools are advertised.
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
fn openai_credentials_and_stale_selections_fail_before_network() {
    let state = state_for("http://127.0.0.1:1/v1");
    let mut backend = backend(&state);
    let mut input = request();
    input.selection = parse_selection("openai/gpt-4.1");
    assert_eq!(
        collect(&mut backend, input.clone()),
        vec![ResponseEvent::Failed(
            "selected gateway is not registered in models.json".into()
        )]
    );
    let configured = json!({"providers":{"openai":{"baseUrl":"https://api.openai.com/v1","api":"openai-completions","models":[{"id":"gpt-4.1"}]}}});
    fs::write(state.join("models.json"), configured.to_string()).unwrap();
    assert_eq!(
        collect(&mut backend, input.clone()),
        vec![ResponseEvent::Failed(
            "OpenAI credential unavailable: run ahi login".into()
        )]
    );
    fs::create_dir(state.join("credentials")).unwrap();
    fs::write(
        state.join("credentials/openai.json"),
        "{\"flow\":\"legacy\"}",
    )
    .unwrap();
    assert_eq!(
        collect(&mut backend, input.clone()),
        vec![ResponseEvent::Failed(
            "run ahi login to migrate to the current OpenAI flow".into()
        )]
    );
    for field in ["baseUrl", "apiKey", "headers", "api"] {
        let mut conflict = configured.clone();
        conflict["providers"]["openai"][field] = match field {
            "baseUrl" => json!("http://127.0.0.1:1/v1"),
            "headers" => json!({"Authorization":"$UNSET_INFERENCE_FIXTURE"}),
            "api" => json!("other"),
            _ => json!("$UNSET_INFERENCE_FIXTURE"),
        };
        fs::write(state.join("models.json"), conflict.to_string()).unwrap();
        let events = collect(&mut backend, input.clone());
        assert!(
            matches!(&events[..], [ResponseEvent::Failed(message)] if message.contains("conflict") || message.contains("browser credentials require") || message.contains("api must be openai-completions")),
            "{events:?}"
        );
    }
    fs::write(state.join("models.json"), json!({"providers":{"local":{"baseUrl":"http://127.0.0.1:1/v1","api":"openai-completions","models":[{"id":"vendor/model"}]}}}).to_string()).unwrap();
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

// ---- Tool loop: policy, approval, execution, and resubmission ----

struct ToolFixture {
    root: PathBuf,
    state: PathBuf,
    workspace: PathBuf,
}

impl ToolFixture {
    fn new(url: &str, actions: &str, model: serde_json::Value) -> Self {
        let root = crate::fixture_directory::create_unique_fixture_directory("chat-tools");
        let (state, workspace) = (root.join("state"), root.join("workspace"));
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir_all(&state).unwrap();
        write_models(&state, url, model);
        fs::write(workspace.join("ahi.actions.yaml"), actions).unwrap();
        fs::write(workspace.join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(workspace.join("src/b.rs"), "fn b() {}\n").unwrap();
        fs::write(workspace.join("secret"), "secret-value\n").unwrap();
        Self {
            root,
            state,
            workspace,
        }
    }

    fn backend(&self) -> HttpBackend {
        HttpBackend::with_workspace(self.state.clone(), self.workspace.clone())
    }
}

impl Drop for ToolFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const APPROVE_READS: &str = "version: 1\nrules:\n\
    - {id: reads, action: tool_call, match: {tool: read_file, path: src}, effect: approve}\n\
    - {id: no-secret, action: tool_call, match: {tool: read_file, path: secret}, effect: deny}\n";

const ASK_ALL: &str = "version: 1\nrules:\n\
    - {id: ask-read, action: tool_call, match: {tool: read_file}, effect: approve, ask: true}\n\
    - {id: ask-list, action: tool_call, match: {tool: list_dir}, effect: approve, ask: true}\n";

fn calls_stream(calls: &[(&str, &str, serde_json::Value)]) -> String {
    let deltas: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (id, name, arguments))| {
            json!({"index": index, "id": id, "type": "function",
                   "function": {"name": name, "arguments": arguments.to_string()}})
        })
        .collect();
    stream(&tool_stream(&[json!(deltas)], "tool_calls"))
}

fn text_stream(text: &str) -> String {
    stream(&format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}]})
    ))
}

/// Collect a turn, answering approvals in order. `None` drops the reply.
/// Returns the non-approval events and the proposed actions of each approval.
fn collect_answering(
    backend: &mut HttpBackend,
    request: ChatRequest,
    mut answers: Vec<Option<Vec<ToolDecision>>>,
) -> (Vec<ResponseEvent>, Vec<Vec<ProposedAction>>) {
    answers.reverse();
    let receiver = backend.submit(request);
    let (mut events, mut approvals) = (Vec::new(), Vec::new());
    loop {
        match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            ResponseEvent::ToolApprovalRequested(approval) => {
                approvals.push(approval.calls);
                match answers.pop().expect("unexpected approval request") {
                    Some(decisions) => approval.reply.send(decisions),
                    None => drop(approval.reply),
                }
            }
            event => {
                let done = matches!(event, ResponseEvent::Completed | ResponseEvent::Failed(_));
                events.push(event);
                if done {
                    assert!(answers.is_empty(), "unused approval answers");
                    return (events, approvals);
                }
            }
        }
    }
}

fn results(events: &[ResponseEvent]) -> Vec<(String, i32, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            ResponseEvent::ToolResult {
                call_id,
                exit_code,
                content,
                ..
            } => Some((call_id.clone(), *exit_code, content.clone())),
            _ => None,
        })
        .collect()
}

fn returns(events: &[ResponseEvent]) -> usize {
    events
        .iter()
        .filter(|event| **event == ResponseEvent::ToolCallReturned)
        .count()
}

#[test]
fn an_approve_rule_runs_the_call_and_resubmits_the_exchange() {
    let (url, server) = server_sequence(vec![
        calls_stream(&[("call_a", "read_file", json!({"path": "./src/a.rs"}))]),
        text_stream("done"),
    ]);
    let fixture = ToolFixture::new(&url, APPROVE_READS, json!({"id": "vendor/model"}));
    let (events, approvals) = collect_answering(&mut fixture.backend(), request(), vec![]);
    assert!(approvals.is_empty());
    assert_eq!(
        events,
        vec![
            ResponseEvent::ToolCallsRequested(vec![super::super::tools::call(
                "call_a",
                "read_file",
                json!({"path": "./src/a.rs"})
            )]),
            ResponseEvent::ToolResult {
                call_id: "call_a".into(),
                tool: "read_file".into(),
                output: "read_file src/a.rs\nfn a() {}".into(),
                content: "fn a() {}\n".into(),
                exit_code: 0,
            },
            ResponseEvent::ToolCallReturned,
            ResponseEvent::TextDelta("done".into()),
            ResponseEvent::Completed,
        ]
    );
    let requests = server.join().unwrap();
    let (first, second) = (request_body(&requests[0]), request_body(&requests[1]));
    let advertised: Vec<_> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(advertised, ["read_file"]);
    assert_eq!(first["tools"], second["tools"]);
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[..3], first["messages"].as_array().unwrap()[..]);
    assert_eq!(
        messages[3],
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": "call_a", "type": "function",
            "function": {"name": "read_file", "arguments": "{\"path\":\"./src/a.rs\"}"}
        }]})
    );
    assert_eq!(
        messages[4],
        json!({"role": "tool", "tool_call_id": "call_a", "content": "fn a() {}\n"})
    );
}

#[test]
fn every_call_gets_a_result_and_diagnostics_never_echo_raw_arguments() {
    let (url, server) = server_sequence(vec![
        calls_stream(&[
            ("c1", "bash", json!({"command": "echo raw-secret"})),
            ("c2", "read_file", json!({"path": "../raw-escape"})),
            ("c3", "read_file", json!({"path": "secret"})),
            ("c4", "read_file", json!({"path": "SECRET"})),
            ("c5", "list_dir", json!({})),
            ("c6", "read_file", json!({"path": "src/a.rs"})),
        ]),
        text_stream("ok"),
    ]);
    let fixture = ToolFixture::new(&url, APPROVE_READS, json!({"id": "vendor/model"}));
    let (events, _) = collect_answering(&mut fixture.backend(), request(), vec![]);
    assert_eq!(
        results(&events),
        vec![
            ("c1".into(), 1, "error: unknown tool".into()),
            (
                "c2".into(),
                1,
                "error: path must be relative to the working directory, without `..`".into()
            ),
            ("c3".into(), 126, "denied: denied by rule no-secret".into()),
            ("c4".into(), 126, "denied: denied by rule no-secret".into()),
            (
                "c5".into(),
                126,
                "denied: no ahi.actions.yaml rule allows this call".into()
            ),
            ("c6".into(), 0, "fn a() {}\n".into()),
        ]
    );
    assert_eq!(returns(&events), 6);
    for event in &events {
        if let ResponseEvent::ToolResult { output, .. } = event {
            assert!(!output.contains("raw-"), "{output}");
        }
    }
    assert_eq!(events.last(), Some(&ResponseEvent::Completed));
    let second = request_body(&server.join().unwrap()[1]);
    let tool_messages = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .count();
    assert_eq!(tool_messages, 6);
}

#[test]
fn operator_decisions_approve_deny_and_grant_for_the_session() {
    let (url, server) = server_sequence(vec![
        calls_stream(&[
            ("c1", "read_file", json!({"path": "src/a.rs"})),
            ("c2", "list_dir", json!({"path": "src"})),
            ("c3", "read_file", json!({"path": "secret"})),
        ]),
        // Covered by the session grant from c1: no second approval.
        calls_stream(&[("c4", "read_file", json!({"path": "src/b.rs"}))]),
        text_stream("done"),
    ]);
    let fixture = ToolFixture::new(&url, ASK_ALL, json!({"id": "vendor/model"}));
    let (events, approvals) = collect_answering(
        &mut fixture.backend(),
        request(),
        vec![Some(vec![
            ToolDecision::ApproveForSession,
            ToolDecision::Approve,
            ToolDecision::Deny,
        ])],
    );
    assert_eq!(
        approvals,
        vec![vec![
            ProposedAction {
                call_id: "c1".into(),
                tool: "read_file".into(),
                target: "src/a.rs".into(),
                grant_prefix: "src".into(),
            },
            ProposedAction {
                call_id: "c2".into(),
                tool: "list_dir".into(),
                target: "src".into(),
                grant_prefix: "src".into(),
            },
            ProposedAction {
                call_id: "c3".into(),
                tool: "read_file".into(),
                target: "secret".into(),
                grant_prefix: "".into(),
            },
        ]]
    );
    assert_eq!(
        results(&events),
        vec![
            ("c1".into(), 0, "fn a() {}\n".into()),
            ("c2".into(), 0, "a.rs\nb.rs".into()),
            ("c3".into(), 126, "denied: denied by the operator".into()),
            ("c4".into(), 0, "fn b() {}\n".into()),
        ]
    );
    assert_eq!(events.last(), Some(&ResponseEvent::Completed));
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn a_session_grant_carries_across_turns_but_not_to_another_gateway() {
    let (url, _first) = server_sequence(vec![
        calls_stream(&[("c1", "read_file", json!({"path": "src/a.rs"}))]),
        text_stream("granted"),
    ]);
    let fixture = ToolFixture::new(&url, ASK_ALL, json!({"id": "vendor/model"}));
    let mut backend = fixture.backend();
    let (_, approvals) = collect_answering(
        &mut backend,
        request(),
        vec![Some(vec![ToolDecision::ApproveForSession])],
    );
    assert_eq!(approvals.len(), 1);

    // Same backend, later turn, same gateway: the grant covers src/ silently.
    let (url, _second) = server_sequence(vec![
        calls_stream(&[("c2", "read_file", json!({"path": "src/b.rs"}))]),
        text_stream("reused"),
    ]);
    let provider = |url: &str| {
        json!({"baseUrl": url, "api": "openai-completions", "apiKey": "gateway",
               "models": [{"id": "vendor/model"}]})
    };
    let write = |url: &str| {
        fs::write(
            fixture.state.join("models.json"),
            json!({"providers": {"local": provider(url), "other": provider(url)}}).to_string(),
        )
        .unwrap()
    };
    write(&url);
    let (events, approvals) = collect_answering(&mut backend, request(), vec![]);
    assert!(approvals.is_empty());
    assert_eq!(
        results(&events),
        vec![("c2".into(), 0, "fn b() {}\n".into())]
    );

    // Outside the granted prefix it still asks.
    let (url, _third) = server_sequence(vec![
        calls_stream(&[("c3", "read_file", json!({"path": "secret"}))]),
        text_stream("asked"),
    ]);
    write(&url);
    let (_, approvals) = collect_answering(
        &mut backend,
        request(),
        vec![Some(vec![ToolDecision::Deny])],
    );
    assert_eq!(approvals.len(), 1);

    // Another gateway would receive the file, so the grant does not apply.
    let (url, _fourth) = server_sequence(vec![
        calls_stream(&[("c4", "read_file", json!({"path": "src/b.rs"}))]),
        text_stream("asked again"),
    ]);
    write(&url);
    let mut other = request();
    other.selection = parse_selection("other/vendor/model");
    let (events, approvals) =
        collect_answering(&mut backend, other, vec![Some(vec![ToolDecision::Deny])]);
    assert_eq!(approvals.len(), 1);
    assert_eq!(results(&events)[0].1, 126);
}

#[test]
fn a_dropped_approval_denies_the_batch_and_ends_the_turn() {
    let (url, server) = server_sequence(vec![calls_stream(&[
        ("c1", "read_file", json!({"path": "src/a.rs"})),
        ("c2", "list_dir", json!({})),
    ])]);
    let fixture = ToolFixture::new(&url, ASK_ALL, json!({"id": "vendor/model"}));
    let (events, approvals) = collect_answering(&mut fixture.backend(), request(), vec![None]);
    assert_eq!(approvals[0].len(), 2);
    assert_eq!(
        results(&events),
        vec![
            ("c1".into(), 126, "denied: approval cancelled".into()),
            ("c2".into(), 126, "denied: approval cancelled".into()),
        ]
    );
    assert_eq!(returns(&events), 2);
    assert_eq!(
        events.last(),
        Some(&ResponseEvent::Failed(
            "tool approval was cancelled; no further calls ran".into()
        ))
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn the_round_cap_returns_results_and_stops() {
    let rounds = super::super::tool_loop::MAX_ROUNDS;
    let responses = (0..=rounds)
        .map(|n| calls_stream(&[(&format!("c{n}"), "read_file", json!({"path": "src/a.rs"}))]))
        .collect();
    let (url, server) = server_sequence(responses);
    let fixture = ToolFixture::new(&url, APPROVE_READS, json!({"id": "vendor/model"}));
    let (events, _) = collect_answering(&mut fixture.backend(), request(), vec![]);
    let outcomes = results(&events);
    assert_eq!(outcomes.len(), rounds + 1);
    assert!(outcomes[..rounds].iter().all(|(_, code, _)| *code == 0));
    assert_eq!(
        outcomes[rounds].2,
        "error: tool round limit reached".to_string()
    );
    assert_eq!(returns(&events), rounds + 1);
    assert_eq!(
        events.last(),
        Some(&ResponseEvent::Failed(format!(
            "model exceeded {rounds} tool rounds in one turn; no further calls ran"
        )))
    );
    assert_eq!(server.join().unwrap().len(), rounds + 1);
}

#[test]
fn a_resubmission_over_max_request_bytes_fails_before_network() {
    let (url, server) = server_sequence(vec![calls_stream(&[(
        "c1",
        "read_file",
        json!({"path": "src/big.txt"}),
    )])]);
    let fixture = ToolFixture::new(
        &url,
        APPROVE_READS,
        json!({"id": "vendor/model", "inputLimits": {"maxRequestBytes": 4096}}),
    );
    fs::write(fixture.workspace.join("src/big.txt"), "x".repeat(8192)).unwrap();
    let (events, _) = collect_answering(&mut fixture.backend(), request(), vec![]);
    assert_eq!(results(&events)[0].1, 0);
    assert_eq!(
        events.last(),
        Some(&ResponseEvent::Failed("model request exceeds inputLimits.maxRequestBytes; shorten the conversation or select another model".into()))
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn tools_never_read_the_state_directory_inside_the_workspace() {
    let (url, server) = server_sequence(vec![
        calls_stream(&[("c1", "read_file", json!({"path": "src/state/models.json"}))]),
        text_stream("done"),
    ]);
    let fixture = ToolFixture::new(&url, APPROVE_READS, json!({"id": "vendor/model"}));
    // Run with the state directory nested inside an approved prefix.
    let nested = fixture.workspace.join("src/state");
    fs::rename(&fixture.state, &nested).unwrap();
    let mut backend = HttpBackend::with_workspace(nested, fixture.workspace.clone());
    let (events, _) = collect_answering(&mut backend, request(), vec![]);
    assert_eq!(
        results(&events),
        vec![(
            "c1".into(),
            1,
            "error: path is inside ahi's state directory, which tools cannot read".into()
        )]
    );
    assert_eq!(events.last(), Some(&ResponseEvent::Completed));
    server.join().unwrap();
}

#[test]
fn earlier_tool_exchanges_replay_on_the_next_turn() {
    let (url, server) = server_sequence(vec![text_stream("again")]);
    let state = state_for(&url);
    let mut assistant = ChatMessage::new(TranscriptRole::Assistant, "checking");
    assistant.tool = Some(ToolPayload::Calls(vec![super::super::tools::call(
        "call_a",
        "read_file",
        json!({"path": "a"}),
    )]));
    let mut result = ChatMessage::new(TranscriptRole::Tool { exit_code: 0 }, "preview only");
    result.tool = Some(ToolPayload::Result {
        call_id: "call_a".into(),
        name: "read_file".into(),
        content: "model content".into(),
    });
    let mut input = request();
    input.messages = vec![
        ChatMessage::new(TranscriptRole::Operator, "first"),
        assistant,
        result,
        ChatMessage::new(TranscriptRole::Assistant, "answer"),
        ChatMessage::new(TranscriptRole::Operator, "next"),
    ];
    assert_eq!(
        collect(&mut backend(&state), input).last(),
        Some(&ResponseEvent::Completed)
    );
    let body = request_body(&server.join().unwrap()[0]);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "checking", "tool_calls": [{"id": "call_a", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}]},
            {"role": "tool", "tool_call_id": "call_a", "content": "model content"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "next"}
        ])
    );
    assert!(body.get("tools").is_none());
    fs::remove_dir_all(state).unwrap();
}
