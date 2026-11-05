# OpenAI browser-login inference

ahi sends a curated `openai/<model>` chat turn to the public OpenAI **Responses API** with the browser-login credential. Gateways speak whichever protocol their `api` declares; see [Providers](PROVIDERS.md). This is implemented transport code, not a plan for a future transport. OpenAI tool requests go through the same policy-gated builtin tool loop as every other protocol; see [Tool calls](TOOL_CALLS.md).

Related contracts: [browser login](OPENAI_LOGIN.md), [gateway model configuration](GATEWAY_MODEL_CONFIGURATION.md), and [chat screen](CHAT_SCREEN.md).

## Mental model

```text
ChatRequest (selection + text history + thinking level)
  → local curation/configuration validation
  → CompletionConnection { CompletionTransport, ModelMetadata }
      Gateway { url, headers } → the declared protocol's adapter
      OpenAiResponses         → pinned Responses adapter, browser-token profile
  → one protocol-neutral turn loop with the policy-gated builtin tools
  → request serialization and byte-limit validation
  → credential validation / bounded refresh
  → HTTP response → bounded SSE events → chat ResponseEvent
  → exactly one Completed or Failed from HttpBackend::submit
```

`CompletionTransport` prevents a gateway URL/header configuration from becoming the destination for a browser credential. Connection and credential types do not implement `Debug`. Secrets are resolved by the request worker, not by drawing the model picker or loading a catalog from disk.

## Destination and credential boundary

- Inference is `POST https://api.openai.com/v1/responses` with `Authorization: Bearer <access token>`.
- Token refresh remains `POST https://auth.openai.com/api/accounts/oauth/token`.
- Both HTTP agents disable redirects. No production environment variable, catalog value, or command-line argument can override these credential destinations.
- The stored credential must pass the existing `chatgpt-direct` validation: issued dynamic client ID, resource `https://api.openai.com/v1`, and `chatgpt.tokens.use.direct` scope. Legacy Codex credentials require a new login. No account-ID extraction or `ChatGPT-Account-Id` header is used.
- The selected model must still exist in the local curated `models.json` entry. A different OpenAI base URL, an API key, custom headers, or an incompatible API declaration fails before credential resolution or networking. Use another gateway ID for proxies.
- The existing configuration's `api: "openai-completions"` is retained for compatibility with curation and gateway configuration parsing. The reserved `openai` ID selects the Responses transport; that legacy configuration spelling does **not** select the wire protocol for browser login.

Authentication uses one shared `authenticated` lifecycle for catalog and inference. An expired credential is renewed before submission. Otherwise, a definitive HTTP 401 can cause one refresh and resubmission. Refresh uses the issued client ID and resource and saves validated rotated credentials with the existing protected atomic replacement. A failed refresh leaves the previous credential bytes intact. If a refresh already happened, another 401 ends the turn with login guidance.

No network error, timeout, redirect, 403, 429, 5xx, malformed stream, refusal, truncation, or failure after content causes automatic generation retry. A successful refresh followed by a rejected inference request retains the successfully refreshed credential; it does not restore a revoked token. Upstream response bodies, transport diagnostics, parser input, and token values are not interpolated into errors.

## Request shape and history

For an ordinary text conversation the payload is equivalent to:

```json
{
  "model": "the-explicitly-curated-model-id",
  "stream": true,
  "store": false,
  "input": [
    {"role": "user", "content": [{"type": "input_text", "text": "First question"}]},
    {"role": "assistant", "content": "Previous answer"},
    {"role": "user", "content": [{"type": "input_text", "text": "Follow-up question"}]}
  ]
}
```

The assistant entry uses Responses' easy-input message form. All existing user/assistant text is replayed on each turn, and complete builtin tool exchanges are replayed as `function_call` and `function_call_output` items; notices are never sent. Tool definitions are declared only when an `ahi.actions.yaml` rule approves a builtin. There is no `previous_response_id` or server-stored conversation, and no invented tool output.

Reasoning is sent as `reasoning: {"effort": "medium"}` only when effective metadata explicitly enables both reasoning and reasoning-effort support and thinking is not Off. Off omits the setting; the literal `off` is never sent. A missing capability does not imply support. The metadata does not describe per-model effort-level maps; a service can reject a level unsupported by that particular model. No reasoning summaries or encrypted reasoning items are requested or replayed in this text-only implementation.

**Browser-token exception:** Pi's current adapter explicitly omits `max_output_tokens`, temperature, and prompt-cache-retention/options fields for Sign in with ChatGPT. ahi follows that boundary, even if model metadata contains `maxTokens`. It does not copy gateway `max_tokens`, `max_completion_tokens`, `reasoning_effort`, or `stream_options` fields into Responses. Gateway output-budget behavior remains unchanged: the 4096-token conservative budget, model ceiling, selected field, and conditional usage/store settings still apply there.

The serialized UTF-8 browser request has a 16 MiB hard limit and also respects a smaller declared `inputLimits.maxRequestBytes`. These checks run before reading credentials or refreshing. Context-window metadata reaches the existing context meter, but no authoritative tokenizer is available: ahi does not claim exact token-window enforcement. Usage and cost remain unknown until reported; complete pricing metadata is required for a cost calculation.

## Stream normalization

Source: [`responses.rs`](../src/chat/backend/http/responses.rs).

| Responses event | ahi behavior |
|---|---|
| `response.created` | Track the response ID and reject conflicting identities. |
| `response.output_item.added` | Open a typed pending output slot, keyed by `output_index`, with a unique bounded item ID. |
| `response.output_text.delta` | Validate item/index association and emit `TextDelta`. |
| Function-argument delta/done | Accumulate bounded arguments for the matching function item; do not display them. |
| `response.output_item.done` | Validate identity, text/arguments and completion; close the slot. Final text can supply text for a part that had no deltas. Conflicting text is not silently repaired. |
| `response.completed` | Require `status: "completed"`; reconcile the ordered terminal output with every finished slot; report actual usage and normalize validated calls. |
| `response.incomplete` | Fail distinctly for output-limit truncation versus other incompleteness. |
| Refusal events/content | Fail with a refusal diagnostic, without rendering refusal text as the answer. |
| `response.failed` / `error` | Fail with a bounded generic diagnostic, not upstream text. |
| Reasoning items/events | Track item completion where applicable; do not show summaries as answer text. |

EOF, `[DONE]`, or an HTTP 200 alone is **not** Responses success. Pending or conflicting output at the terminal event fails. Unknown auxiliary events are ignored; unsupported output item types fail closed rather than being treated as executable requests. Completion does not depend on a Chat Completions finish reason.

Limits: 1 MiB per SSE line/event, 16 MiB total stream, 128 output slots, bounded content indexes and identifiers, at most 32 validated function calls, and 64 KiB argument text per call. JSON object arguments, valid function names, contiguous normalized call indexes, and unique execution call IDs are required by the existing `ToolCallAssembler` boundary. Network byte fragmentation, including multibyte Unicode, is handled before JSON parsing. HTTP status must be 200. Content-Type alone is not an acceptance gate: the parser validates actual SSE framing, typed events, and successful terminal reconciliation, matching the OpenAI SDK's streaming behavior. A correctly completed SSE response works even with an incorrect or missing media-type header. Ordinary JSON, HTML/access pages, malformed events, and truncated streams still fail; there is no nonstreaming-JSON success fallback.

A valid function-call batch produces `ToolCallsRequested` and the existing safe name/count notice, then enters the shared tool loop: without an approving rule the turn fails with `tool execution is disabled`; with one, each call is decided, run or denied, and its result is appended before the continuation request. No arguments are printed and no result is fabricated. An incomplete or malformed batch never reaches that boundary. The request worker alone sends the terminal screen event; parsers return a `StreamOutcome` rather than sending `Completed` themselves.

## Evidence from Pi

Protocol selection was checked against the installed **Pi release 1.4.0** JavaScript and embedded TypeScript source maps, not inferred from the word “OpenAI.” Upstream links below identify the corresponding maintained source paths; `main` may advance after the inspected release.

1. [`packages/ai/src/auth/oauth/openai-chatgpt.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/auth/oauth/openai-chatgpt.ts): `loginOpenAIChatGPT`, `credentialFromTokenResponse`, `refreshAccessToken`, and `openaiChatGPTOAuth.toAuth` establish the direct resource/scope, dynamic issued client, access-token bearer resolution, and refresh contract.
2. [`packages/ai/src/providers/openai.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/providers/openai.ts): `openaiProvider` combines that OAuth implementation with `https://api.openai.com/v1` and the Responses adapter. Its classifier filter notes that subscription tokens do not authorize the Decisions API.
3. [`packages/ai/src/api/openai-responses.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-responses.ts): `isChatGPTSignIn`, `buildParams`, and `createClient` establish `store:false`, the browser-token unsupported-field omissions, nested reasoning settings, and `client.responses.create` transport.
4. [`packages/ai/src/api/openai-responses-shared.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-responses-shared.ts): `convertResponsesMessages` and `processResponsesStream` establish output-item identity, text/function streaming, terminal response checks, and usage fields. ahi deliberately reports refusals separately rather than adopting Pi's refusal-as-text presentation.
5. [`packages/ai/src/auth/oauth/openai-codex.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/auth/oauth/openai-codex.ts) is the distinct legacy flow with account-ID extraction; it is not the authority for this token's destination.

Pi's [Models](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/models.md), [Models Configuration](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/models-configuration.md), and [Custom Providers](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/custom-provider.md) explain the API/capability boundary and exactly-one terminal event pattern. This implementation is original Rust, not a copied provider implementation.

## Safe local verification and troubleshooting

The maintained verification uses disposable state and real loopback HTTP; it must not read an actual browser credential or make live inference calls:

```sh
cargo fmt --all -- --check
cargo clippy --offline --target-dir target/openai-inference-verification --all-targets --all-features -- -D warnings
cargo test --offline --target-dir target/openai-inference-verification --all-features
cargo build --offline --target-dir target/openai-inference-verification
uv run --offline scripts/smoke-model-picker.py \
  --binary target/openai-inference-verification/debug/ahi \
  --capture-directory target/openai-inference-captures
RUSTDOCFLAGS='-D warnings' cargo doc --offline \
  --target-dir target/openai-inference-verification --all-features --no-deps
cargo package --offline --allow-dirty \
  --target-dir target/openai-inference-package-verification
git diff --check
```

The existing Rust HTTP fixture cases now also exercise Responses request serialization, real streamed Unicode/history/usage, expiry refresh followed by stream consumption, 401 refresh and bounded reauthorization, secret-safe failures, byte limits, malformed/truncated streams, and function-call refusal. The same valid stream is checked under normal, incorrect, and missing Content-Type headers. JSON error/completed documents and HTML are rejected; synthetic private markers in bodies and arbitrary headers never enter diagnostics or events. The loopback credential-destination helper exists only under `cfg(test)`. The production pinned request wrapper and fixture helper share the same private request/authentication implementation.

The PTY workflow uses a disposable state directory and tests missing/legacy OpenAI credentials, login cancellation, transcript retention, and gateway/model metadata regressions. It does **not** prove a successful browser-token request in the shipped binary. Its macOS PTY fork-before-server-thread ordering is retained. Port 1455 must be free for login regressions.

- **Credential unavailable / migration:** use a separately authorized `ahi login` workflow; editing token JSON is not a repair procedure.
- **Configuration conflict:** restore the public OpenAI base URL and remove custom key/headers, or use a separate gateway ID. Do not override the credential destination for diagnosis.
- **Permission/rate-limit failure:** inspect the HTTP status and account permissions through approved provider interfaces. No fallback to a gateway or legacy Codex endpoint occurs automatically.
- **Interrupted or truncated output:** partial text can remain visible; the turn is failed, not completed. Retry only by an explicit user decision because a network failure may have followed generation.
- **Unexpected response format:** diagnostics include HTTP status and an allowlisted Content-Type category, never arbitrary header values or raw body text. A non-SSE JSON-like or HTML/markup body is identified from a bounded prefix, not interpreted as a model answer or an upstream error message. Share only this diagnostic when investigating; do not attach credentials or complete responses. The original `OpenAI response must be text/event-stream` gate was reproduced locally with valid SSE labeled `application/json` and removed. This establishes a local false rejection, not the body/header returned by any particular live request.

**Verification boundary:** no live OpenAI call or real credential access was performed for this implementation. Pi source establishes the matching protocol for this OAuth flow; disposable fixtures establish local request/stream/auth behavior. They do not prove that a particular live grant, subscription, model ID, reasoning effort, quota, or account policy currently permits a successful OpenAI conversation. Responses text history is replayed, but native reasoning signatures and Responses tool-call/result history are not persisted or continued; those are deliberately gated future work described in [Tool calls](TOOL_CALLS.md). Gateway builtin exchanges do continue and replay in memory; no transport persists conversations.
