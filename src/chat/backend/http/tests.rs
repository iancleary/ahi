use super::*;
use crate::chat::backend::parse_selection;
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let content_type = content_type
        .map(|value| format!("Content-Type: {value}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
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
fn openai_credentials_and_stale_selections_fail_before_network() {
    let state = state_for("http://127.0.0.1:1/v1");
    let mut backend = HttpBackend::new(state.clone());
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
