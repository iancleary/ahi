# Tool calls: parsing is not permission

A model-issued tool call is **untrusted data describing a requested operation**. Parsing complete JSON proves neither that the arguments match an application schema nor that the operator authorized the operation. A supported protocol or model capability cannot grant filesystem, process, network, or secret access.

ahi implements streaming function-call assembly and structural validation for three wire protocols: OpenAI Chat Completions, OpenAI Responses (gateways and the browser-login credential), and Anthropic Messages. One protocol-neutral turn loop (`src/chat/backend/http.rs`) runs the builtin read-only `read_file` and `list_dir` tools for every protocol when the working directory's `ahi.tools.yaml` approves them, with operator decisions for `ask: true`. Each protocol supplies only its request body, stream decoder, and tool-definition shape (`src/chat/backend/protocol/`). This page distinguishes that implemented subset from the future general agent runtime; the roadmap does not authorize additional execution.

See [OpenAI inference](OPENAI_INFERENCE.md) for the implemented transport and verification boundary, [chat screen](CHAT_SCREEN.md) for rendering, and [gateway model configuration](GATEWAY_MODEL_CONFIGURATION.md) for capability metadata.

## What exists today

- [`ToolCallAssembler`](../src/chat/backend/tool_call.rs) assembles indexed gateway deltas and validates IDs, names, batch cardinality, argument sizes, and complete JSON-object arguments. `push_function` brings completed Responses function items through that same boundary.
- [`responses.rs`](../src/chat/backend/protocol/responses.rs) correlates Responses `output_index` and item ID, accumulates incremental argument text, reconciles argument completion and output-item completion, and requires a matching successful terminal response. Text and reasoning events do not become arguments or execution output.
- [`ResponseEvent::ToolCallsRequested`](../src/chat/backend/mod.rs) carries structurally validated requests. The event is not authorization. Gateway calls then cross the builtin typed-argument and policy boundary in [`tool_loop.rs`](../src/chat/backend/tool_loop.rs); [`ChatScreen`](../src/chat/mod.rs) owns the single-use approval reply channel.
- Definitions are advertised only for approved builtins, on every protocol; absent approval, requested calls fail with `tool execution is disabled`. Responses continues with `function_call`/`function_call_output` items and Anthropic with `tool_use`/`tool_result` blocks. No adapter launches a shell.
- Invalid, conflicting, duplicate-ID, oversized, or incomplete calls fail before that event. Argument text and parser details do not appear in diagnostics or the UI notice.
- Every call produces one `ToolResult` and `ToolCallReturned`, including denial/error outcomes. Structured `ToolPayload` transcript entries replay complete assistant-call/result exchanges; pairing is shared ([`protocol/replay.rs`](../src/chat/backend/protocol/replay.rs)) and serialization is per protocol. A call without its result, or a result without its call, is dropped from replay, never re-executed. A dropped approval reply cancels the whole batch, including calls a rule alone had approved.

The loop is bounded to 8 rounds and 256 KiB of returned tool content per turn (every result is charged, and content past the budget is cut and replaced by one short notice per call), checks serialized request byte limits on every continuation, and confines builtin reads with software path validation and state-directory exclusion. It is **not an OS sandbox**; there is no durable journal, general process executor, or native Responses tool replay. See [Chat screen](CHAT_SCREEN.md#tool-calls) for arguments, policy precedence, approval keys, session grants, path restrictions, and output limits, and [Agent runtime](AGENT_RUNTIME.md#implemented-subset-tool_call) for the implemented policy schema.

Bounds are 32 calls and 64 KiB argument text per call, within the Responses stream's 1 MiB event / 16 MiB stream limits. Responses also bounds output slots and content indexes. Partial arguments are never treated as a valid object just because a prefix happens to parse. A successful terminal response and completed output items are required before a batch crosses the boundary.

## Pi evidence and what to adopt

The evidence below is the installed **Pi release 1.4.0** JavaScript and, where inspected, embedded TypeScript in its source maps. Links identify upstream `packages/ai/src/...` and `packages/agent/src/...` paths, not machine-local files. The linked `main` branch may evolve; this is a release-specific reading, not a claim to have fetched current upstream HEAD. No provider code was copied into Rust.

| Pi source and named functions/types | Pattern and Rust consequence |
|---|---|
| [`packages/ai/src/types.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/types.ts): `ToolCall`, `AssistantMessage`, `ThinkingContent`, `ToolResultMessage`, `AssistantMessageEvent` | A turn has ordered text/thinking/tool blocks, provider/model identity, usage and a concrete stop reason. Results refer to a call, not merely a tool name. Use typed blocks and correlated result records rather than concatenating calls into assistant text. |
| [`packages/ai/src/api/openai-responses-shared.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-responses-shared.ts): `processResponsesStream`, `convertResponsesMessages`, `convertResponsesTools` | Assemble by output slot; distinguish the provider item ID from `call_id`. Pi's normalized ID combines them with `|`, and its serializer separates them again. Reasoning signatures and final response output can supply replay state. Rust should use separate fields/newtypes rather than delimiter parsing. |
| [`packages/ai/src/api/openai-completions.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-completions.ts): `stream`, `convertMessages` | Streamed `tool_calls[index]` fragments are assembled before completion. Replay an assistant `tool_calls` entry and one `role:"tool"` result carrying `tool_call_id`. Preserve endpoint compatibility flags instead of assuming all gateways have identical replay rules. |
| [`packages/agent/src/agent-loop.ts`](https://github.com/earendil-works/pi/blob/main/packages/agent/src/agent-loop.ts): `runLoop`, `streamAssistantResponse`, `failToolCallsFromTruncatedMessage` | Separate streaming a complete assistant turn from running its calls and continuing the model. Error/abort stops the loop. Truncated calls are not executed, even if their arguments happen to parse. ahi's gateway builtin loop continues with correlated results; Responses still stops. |
| Same agent source: `prepareToolCall`, `runToolCall` | Resolve the registry entry, prepare/validate arguments, consult `beforeToolCall`, check cancellation, and only then create a prepared execution. Nested calls must pass the same gate; direct executor access must not bypass policy. |
| Same agent source: `executeToolCallsSequential`, `executeToolCallsParallel`, `executePreparedToolCall`, `finalizeExecutedToolCall`, `createToolResultMessage` | Model batching, execution, updates, failure normalization and transcript results are separate phases. Pi orders parallel batch results and rejects updates after completion. Rust needs one result owner per call and explicit partial-batch recovery, not a retry of the whole batch. |
| [`packages/ai/src/api/openai-responses.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-responses.ts): `buildParams`, `isChatGPTSignIn` | Responses and browser-token request restrictions remain transport policy. Do not copy Chat Completions request fields, reasoning settings, or legacy Codex authentication into this adapter. |

Pi's [Custom Providers](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/custom-provider.md) specifies balanced stream events, complete parsed arguments at `toolcall_end`, and exactly one terminal done/error. Its [Models Configuration](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/models-configuration.md) distinguishes Responses and Chat Completions compatibility. Its [security guidance](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/security.md) explicitly says tool access normally inherits the running user's permissions: project trust, approval hooks, and a working directory are not an isolation boundary. Adopt the lifecycle ideas, **not** an assumption that a Pi hook supplies an operating-system sandbox.

## Identity and incremental assembly

There are three independent identities:

1. **Turn identity**: the local generation/batch, provider, model and protocol that produced the request.
2. **Provider item identity**: Responses `item.id` (for example `fc_item_1`) plus `output_index`, used to correlate stream fragments and replay provider-native output.
3. **Execution call identity**: Responses `call_id` (for example `call_operation_1`), or Chat Completions `tool_calls[].id`, used to correlate exactly one result.

A tool's registry name is none of these. Two calls to `read_file` need distinct call IDs and independent authorization/results. An output index is local to one response and must not become a durable execution key.

Implemented Responses slots transition from `Pending` to `Finished`. Argument deltas must identify the pending function item; a completed argument payload cannot conflict with its prefix, and item completion cannot change a completed argument payload. Final output must match the finished slots in order. Provider item IDs and execution call IDs are separately checked for duplicate/conflicting use; normalized function calls are contiguous even when reasoning/text output occupies other indexes. The current `ToolCall` retains the execution call ID only; the terminal output items themselves, native IDs and reasoning included, ride alongside the calls as `NativeReplay` for the continuation request. Nothing is persisted.

For future replay, introduce distinct `TurnId`, `CallId`, and `ResponsesItemId` newtypes, plus `ProviderOutputIdentity { provider, model, protocol, response_id, item_id }`. Do not squeeze provider IDs into the current validated execution ID field or change its character restrictions merely to accommodate `|`.

Example stream:

```text
output[0] function_call: id=fc_a, call_id=call_a, name=read_file
output[1] function_call: id=fc_b, call_id=call_b, name=read_file
arguments delta for fc_b → {"path":
arguments delta for fc_a → {"path":"src/main.rs"}
arguments done for fc_a → complete object text
output item done for fc_a
arguments delta for fc_b → "Cargo.toml"}
output item done for fc_b
response completed → reconcile all items → validate whole batch
```

Neither call is executable while arguments are arriving. Reject duplicate slots, reused item IDs, changing item/call/name identity, duplicate execution IDs, malformed object arguments, missing completion, truncation or a failed terminal response. Do not execute the first valid call while waiting to discover that another call in the same model turn is incomplete. The current parser emits no partial batch to policy.

## Proposed protocol-neutral turn and replay boundary

**Not implemented:** replace text-only chat history with an ordered `AssistantTurn`:

```text
AssistantTurn {
    turn_id, origin: ProviderModelProtocol,
    blocks: [Text | Reasoning | ToolRequest],
    termination: Stop | ToolRequests | Truncated | Failed | Cancelled,
    usage: ReportedUsage?,
}
ToolRequest { call_id, name, object_arguments, provider_identity? }
ToolResultRecord { turn_id, call_id, outcome, bounded_content }
Reasoning { display_summary?, opaque_provider_item?, origin }
```

An opaque reasoning item is transport data, not an instruction or display text. Bound its size, retain origin and integrity, and never print encrypted/native content in ordinary logs. Capture complete native output at the response boundary, including terminal-only reasoning information, before discarding streaming buffers. Pi's `processResponsesStream` backfills reasoning signatures from terminal output for this reason.

Serialization must be explicit per protocol:

| Data | Chat Completions | Responses |
|---|---|---|
| Assistant request | `role:"assistant"`, `tool_calls:[{id,type:"function",function:{name,arguments}}]` | `type:"function_call"`, `call_id`, `name`, serialized `arguments`; native item `id` only where replay-compatible |
| Correlated result | `role:"tool"`, `tool_call_id`, bounded content (and name only when required) | `type:"function_call_output"`, `call_id`, bounded output |
| Text | Assistant string content | Assistant message/output-text item, preserving supported native identity/phase |
| Reasoning | Only the gateway's explicitly supported representation | Only supported same-origin native reasoning items/signatures; never ordinary answer text |

Always serialize validated structured arguments, not unfinished stream buffers. Replaying a transcript is not permission to execute it again. Never manufacture a tool success to satisfy pairing constraints. On a model/protocol switch, preserve local call/result correlation and apply an explicit provider conversion policy. Pi's `convertResponsesMessages` drops or normalizes incompatible item identity on foreign/different-model replay; Rust should encode that rule, preserve audit identity separately, and reject unsupported unresolved histories instead of blindly forwarding encrypted items.

All three protocols replay complete structured builtin exchanges. Responses and Anthropic also replay the provider-native output a tool-calling turn needs (reasoning items with encrypted content, signed thinking blocks) verbatim and only to the protocol that produced it. The proposed richer turn model is future work for protocol-neutral identity and durable recovery, not a prerequisite retroactively claimed by the current subset.

## Proposed registry, schema and authorization

**Not implemented:** own a `ToolRegistry` in the application, not in a discovery response or model output. Each allowlisted `ToolDefinition` should contain a stable name/version, description, parameter schema, typed argument decoder, capability requirements, execution mode, isolation profile, limits, and executor binding. Provider schemas are derived from that registry; a gateway claiming “tools supported” does not install a definition.

Suggested boundaries:

```text
untrusted streamed bytes
 → structurally complete ToolRequest
 → registry lookup + versioned schema validation
 → ValidatedInvocation
 → per-call AuthorizationDecision
 → AuthorizedInvocation (contains a scoped capability grant)
 → isolated executor
 → exactly one ToolResultRecord
 → record results / render safe progress
 → optionally request another model turn
```

Responses JSON-object validation is **not** registry/schema validation. The gateway builtin registry already decodes tool-specific arguments and rejects unknown fields/tools and unsafe paths. A future general registry must also reject unknown tools, unknown fields where prohibited, invalid types/ranges, dangerous path forms, and unsupported schema features. Decode into tool-specific Rust argument types after validation. If arguments are normalized, authorize the exact normalized invocation that will execute; do not authorize one path and later rewrite it to another.

Authorization belongs to operator/application policy and must run for each call, including nested calls and every member of a batch. The grant should bind turn/call ID, tool version, canonical arguments or digest, workspace, required operations, isolation profile and expiration. Model-generated text, registry membership and prior approval of another call are not authority. A default-deny decision can be `Denied`, `RequiresConfirmation`, or `Authorized(scoped_grant)`; awaiting confirmation is not running.

Do not initially expose a generic shell tool. Start, if separately authorized, with a narrow real operation whose filesystem/process/network effects can be contained and observed.

## Proposed executor and state machine

**Not implemented:** use a tagged execution state, not booleans such as `approved`, `running`, and `finished` that can contradict one another:

```text
Pending(ValidatedInvocation)
  → AwaitingAuthorization
      → Denied(ResultRecord)
      → Authorized(ScopedInvocation)
          → Running { execution_handle, deadline, output_budget }
              → Succeeded(ResultRecord)
              → Failed(ResultRecord)
              → Cancelled(ResultRecord)
```

Timeout is a concrete failure outcome; cancellation records whether effects may already have occurred. A call that was never started must not claim it executed. Structural/model failures before authorization fail the turn without scheduling any call. Denied or cancelled requests may eventually have correlated failure results for provider continuation, but those results describe policy/execution facts, not invented model output.

Only a result owner that consumes the running state can finalize a call. Insert results using a unique `(turn_id, call_id)` key; reject a second finalization. Progress updates are bounded, separate from final output, and ignored after finalization. Return values, errors and cancelled invocations all produce exactly one final record once admitted into the execution batch.

A working directory alone provides no containment. The isolation profile must constrain:

- workspace-relative file access, traversal, symlink escapes, races, allowed mounts, read/write separation and maximum file sizes;
- executable allowlists and argument boundaries, process groups, child-process termination and exit status;
- environment allowlists, secret removal, credential paths and inherited file descriptors;
- network access, external services and paid operations;
- monotonic deadlines, cancellation propagation, maximum concurrency, memory/CPU/output limits and bounded diagnostic text.

The executor must not inherit the OpenAI credential store or the parent's unrestricted environment. Use actual OS/container/VM controls appropriate to the platform; instructions, schema checks and UI confirmation cannot replace them. Enforcement and recovery need matching-surface verification before any executor is called a sandbox.

## Proposed batch continuation and failure handling

**Not implemented:** create a `ToolBatch` holding the finalized assistant turn and ordered call ledger. Validate the entire batch first; authorize calls individually; start only authorized invocations within configured concurrency. Tools with shared mutable effects require serial execution even if other tools permit parallel execution. Parallel completion order may differ from transcript order; preserve the original call order when recording provider results.

Continue the model only when all admitted calls have exactly one terminal result recorded and replay pairing is valid. No successful result may disappear because another call failed. If one of three calls succeeds, one fails and one is cancelled:

1. record each outcome with its original call ID;
2. close the batch and expose the actual partial outcome;
3. do not rerun the successful call;
4. ask policy/user whether a continuation or a new explicit attempt is allowed.

A model or transport failure after effects must not cause a whole-batch retry. Cancellation stops new scheduling, cancels running handles, drains final records, and prevents continuation until the ledger is consistent. An ambiguous executor disconnect needs an explicit `effect_unknown` failure detail and reconciliation, not an automatic retry.

A model switch is a turn boundary. Do not send a batch with outstanding calls to a different model. Complete/cancel and record the batch first, then serialize a compatible history or reject the switch with guidance. Provider item IDs and reasoning signatures may be model-specific; local execution IDs/results remain immutable. The same rule applies to a failed continuation after a partial parallel batch.

## UI, transcript and future persistence

Current request notices expose safe names/counts, not raw arguments. Gateway approval UI separately shows normalized builtin targets and deliberate operator decisions. Preserve this distinction. A future general approval UI should separately show a bounded, escaped description of the exact proposed effect and authorization scope; an argument preview is a deliberate privileged view, not a normal error string. Render start/progress/final status without claiming execution merely because a model requested it. Do not confuse one model response's completion with completion of an entire tool-and-continuation cycle.

The UI should consume immutable events and identify them by turn/call. Result updates must not split an active assistant response or consume the operator's draft. Existing chat tests cover notices, tool returns, drafts and transcript retention. Real gateway HTTP fixtures and PTY approval/denial workflows additionally exercise the builtin executor; they do not establish general isolated execution.

**Future persistence** needs a versioned append-only turn/call/result journal with protected storage, bounded content and atomic state transitions. Persist intent/grant/start/final facts before allowing continuation where recovery requires it. On restart, running entries are unresolved until reconciled; never execute a saved request merely by replaying history. Store provider-native replay items separately from UI-rendered text. Keep secrets and credential files out of the journal, and make export/redaction policies explicit. Durable effects cannot generally be made exactly-once by a local journal alone; idempotency or reconciliation must be designed per tool.

## Ordered implementation roadmap

The following general-runtime steps require separate authorization; none is enabled by this documentation. The narrow gateway builtin implementation covers parts of schema validation, policy, results and continuation without claiming this complete architecture:

1. **Turn representation and replay:** introduce typed identities and ordered assistant blocks, concrete termination, provider-native replay envelopes and correlated results. Verify real local multi-turn HTTP serialization for both protocols, including model switches and reasoning-item restrictions. No execution.
2. **Registry and schemas:** define a minimal application-owned allowlist, typed argument decoding and provider-schema generation. Verify unknown/malformed requests fail before scheduling. Keep the current disabled diagnostic until every later gate exists.
3. **Authorization:** implement per-call decisions and capability-bearing authorized invocations, including confirmation/cancellation and nested-call routing. Bind policy to the exact workspace and normalized invocation.
4. **Isolated executor:** choose a narrow operation, implement enforced platform boundaries and limits, and verify real observable allowed/denied effects with disposable data. No general shell fallback.
5. **State/result ledger:** implement deadlines, cancellation, update closure, exactly-one result ownership, bounded outputs and partial-batch accounting. Verify real failure/recovery cases rather than internal method-call assertions.
6. **Continuation controller:** append complete result batches before a new request. Verify multiple/interleaved calls, failed partial batches, truncation, cancelled execution, failed continuation and explicit model switches. No automatic repetition of effects.
7. **UI and persistence:** add deliberate authorization/result views, versioned recovery journal, redaction/export policy and restart reconciliation. Verify with the maintained PTY workflow and real isolated operations before claiming full agent behavior.

## Verification and present limitations

The existing real-HTTP Rust fixture tests were extended rather than adding new unit tests. They exercise interleaved Responses function calls, Unicode argument fragments, complete object arguments, duplicate call IDs, invalid names, non-object arguments, pending output at completion, gateway compatibility and safe execution-disabled behavior. Shared auth fixtures verify refresh/retention; the maintained PTY verifies user-visible missing/legacy credentials and existing safe notices/draft retention through its existing workflows.

Run the commands in [OpenAI inference](OPENAI_INFERENCE.md#safe-local-verification-and-troubleshooting). These checks prove local transport/parser/UI boundaries and the gateway builtin model/tool/model loop, including approval, denial and replay. They do not prove live OpenAI acceptance, Responses tool execution, OS-enforced isolation, durable tool history, exactly-once external effects, or a general agent loop. Those remain unimplemented.
