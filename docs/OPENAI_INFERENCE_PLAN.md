# OpenAI chat inference and tool-call implementation plan

Status: completed sequential implementation brief and decision record. See [OpenAI inference](OPENAI_INFERENCE.md) and [Tool calls](TOOL_CALLS.md) for the maintained architecture and operational documentation. The gateway-model feature is the prerequisite; its local formatting, Clippy, existing Rust tests, build, and PTY workflows passed lead verification before this task starts.

## Requested outcome

Replace the chat error `OpenAI browser-login inference is not implemented yet; select a configured OpenAI-compatible gateway` with an actual streaming OpenAI transport using ahi's existing browser-login credential. As part of that implementation, document the Rust tool-call architecture using Pi's maintained documentation and installed source as evidence. Tool execution is not authorized by a model response and must remain disabled in this task.

This is a local implementation, not permission to use real credentials, send paid inference requests, modify live gateway configuration, install dependencies, commit, or publish.

## Four-part delivery checkpoint

- [x] **Blocking first steps:** confirm the existing credential audience/scopes and inspect Pi's current ChatGPT authentication, Responses implementation, and tool lifecycle before selecting a transport.
- [x] **Independent workstreams:** one sequential source owner implements the coupled authentication/transport/stream changes and a source-cited tool design. The lead owns review and alignment of existing documentation.
- [x] **Shared mutable state:** preserve all earlier dirty work; use disposable credentials and local HTTP fixtures only. No production endpoint override may redirect a real browser token to an arbitrary server.
- [x] **Smallest safe decomposition:** typed transport routing → protected credential resolution/refresh → Responses stream normalization → observable fixture verification → Rust tool lifecycle map with execution gates explicit.

## Transport and authentication contract

1. The `openai` provider is reserved. Require an explicitly curated OpenAI model and the existing valid credential contract; reject stale selection or incompatible configuration before network submission.
2. Inspect the credential actually issued by ahi. It uses dynamic-client browser authorization, resource `https://api.openai.com/v1`, and `chatgpt.tokens.use.direct`. Do not assume legacy Codex tokens, extract an account ID from an unrelated JWT, or switch to ChatGPT's Codex backend merely because a Pi example does so.
3. Use the current Pi ChatGPT source to establish the matching inference API and request semantics. Record the chosen endpoint, request shape, and source evidence. If that source does not establish acceptance of ahi's token type, distinguish that uncertainty from verified local protocol behavior.
4. Introduce an explicit transport discriminant rather than letting a generic gateway connection contain incompatible credentials/API assumptions. Resolve secrets only inside the request lifecycle; do not derive Debug or log payloads/headers/credentials.
5. Reuse current credential validation, expiry checks, token refresh and atomic protected persistence. Preserve old credentials on refresh failure. Bound refresh attempts. Never automatically retry a generation after emitted content or an ambiguous transport failure; do not duplicate turns.
6. Pin credentialed inference to its validated public OpenAI destination. No redirects, discovery-provided credential destinations, arbitrary environment URL overrides, or local-debug secret exfiltration paths in the shipped binary.

## Streaming and conversation contract

Normalize the selected protocol into the existing chat events: streamed text, reported usage, validated tool requests, concrete completion, or actionable secret-safe failure. Bound event/response sizes and require the protocol's successful terminal state; EOF is not success. Handle failed/incomplete responses, malformed events, Unicode fragmentation, multiple turns, and a single terminal application event. Do not treat reasoning summaries, refusals, or function arguments as ordinary assistant text.

Preserve conversation messages, current gateway behavior, metadata-based limits and accounting, Commands/menu handoffs, transcript and active-response drafts. Keep `store:false` where the selected API supports it. Use protocol-correct reasoning configuration, not Chat Completions fields copied into Responses. No synthetic token usage, context capacity, pricing, or replay of tool execution.

## Tool-call design artifact

Create `docs/TOOL_CALLS.md`, teaching the mental model and tracing Pi's source concepts into Rust types and lifecycle states. Include concrete source references, implemented versus proposed behavior, examples, operational limits, and an ordered implementation plan. Cover:

- Streaming call assembly versus complete validated JSON arguments; provider item identifiers versus execution call identifiers; parallel/interleaved calls; duplicate/conflicting identifiers and terminal validation.
- A protocol-neutral assistant turn containing text, reasoning blocks when supported, and tool requests; conversion to Chat Completions and Responses input/output items without losing identifiers or inventing results.
- Registry/schema ownership, allowlisted tool definitions, argument validation, authorization decisions, execution isolation, working-directory and filesystem boundaries, environment/secret restrictions, cancellation/timeouts and output limits.
- Typed pending/authorized/running/succeeded/failed/cancelled states; exactly one result per authorized call; model continuation only after results are recorded; handling partial parallel batches and model switches.
- UI notices versus transcript replay and durable storage. Existing tool notices must not disclose arguments. Tool support metadata does not grant authority.

Implement transport-level parsing and normalization needed for OpenAI tool requests, reusing the existing validated `ToolCall` boundary where appropriate. Stop at a clear tool-execution-disabled event/error. Do not add a shell runner, filesystem tools, authorization bypass, or agent execution loop in this task.

## Verification and handoff

Use existing real-HTTP Rust fixture tests for the new transport and authentication behavior; update obsolete unsupported-OpenAI expectations rather than removing the gate. Do not add new unit tests. Extend the maintained PTY runner where appropriate for user-visible errors/regressions. Loopback endpoints may be supplied to internal fixture code, not exposed as production credential-destination overrides.

Run formatting, isolated-target Clippy, all existing Rust tests, build, the full maintained PTY workflow, rustdoc, package verification, and diff checks. Verify the actual Responses request/stream lifecycle with disposable HTTP fixtures, including refresh, failure/secret retention, history, usage, malformed/truncated response, and tool-call refusal. Do not present fixture evidence as a successful live OpenAI conversation.

Deliver changed files, chosen types and protocol, Pi evidence, verified behavior, unresolved limitations, and a concrete command to run the new build. The lead will independently review and verify before reporting completion.

## Completion record

A fresh sequential `feature` delegate implemented the transport after the model-configuration feature passed lead verification. The lead reviewed the diff, compared protocol decisions with installed Pi 1.4.0 source, and independently reran formatting, Clippy, all 167 existing Rust tests, build, the full PTY workflow, rustdoc with warnings denied, and offline package verification. All passed. A final bounded review follow-up corrected obsolete gateway URL guidance and reran the build/PTY workflow successfully.

The selected design is `CompletionTransport::{Gateway, OpenAiResponses}`, with a pinned `https://api.openai.com/v1/responses` target and shared protected `authenticated` lifecycle for catalog and inference. It replaces the unsupported-OpenAI error with actual transport code, not an echo or another gateway fallback. A definitive pre-stream 401 may refresh/resubmit once; ambiguous or post-stream failures never retry generation. Existing HTTP tests were extended with actual loopback request/stream evidence, without adding unit-test cases or exposing a production endpoint override.

The source-cited [tool-call map](TOOL_CALLS.md) documents implemented complete-call parsing and a separately gated roadmap for turn representation, schema registry, per-call authorization, isolated execution, result ledger, continuation, UI and recovery. Tools remain disabled. Browser-token live inference, account/model permission, native reasoning replay, tool execution/continuation, and Windows terminal behavior are not established by the local evidence. No real credentials, live inference requests, commits, or publication were used.
