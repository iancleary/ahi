# Improvement plan: model calls, tool calls, providers, speed

Status: implementation record for the `feat/provider-protocols-agent-loop` branch, written while the work was done. The **Decisions** section lists every call the implementer made that the owner would normally have been asked about. Review those first.

Inputs: an `/improve-my-codebase` pass over `src/harness*`, `src/chat/backend/*`, `src/chat/mod.rs`, and the docs; an independent GPT-6-Astra (Codex CLI) architectural review, recorded in [Codex review](#codex-review); and the data-oriented-design prompts the owner pasted (flat arrays over maps, indexes over pointers, bitsets over `Vec<bool>`, inline storage for small bounded collections, redesign instead of caches).

## Decisions

Each entry: what was decided, why, and what to revisit.

### D1. No Claude subscription (Pro/Max) OAuth transport

Pi's `packages/ai/src/auth/oauth/anthropic.ts` and `api/anthropic-messages.ts` were read as the evidence source, the same way the OpenAI flow was. That path only works by presenting the client as Claude Code: `user-agent: claude-cli/<version>`, `x-app: cli`, the beta flags `claude-code-20250219` and `oauth-2025-04-20`, the `user:sessions:claude_code` scope, and a mandatory first system block `You are Claude Code, Anthropic's official CLI for Claude.` That is impersonation of a first-party client to use a consumer subscription outside the product it is sold for, and it is the mechanism Anthropic has used to cut off third-party harnesses. **It was not implemented.**

What was implemented instead: a full **Anthropic Messages** transport (`api: anthropic-messages`) that authenticates with an API key from the environment (`x-api-key`), a built-in `anthropic` provider that appears when `ANTHROPIC_API_KEY` is set, standard `GET /v1/models` catalog refresh, and the `AuthScheme` seam so an officially supported OAuth grant can be added as one more variant without touching the protocol code. Revisit if Anthropic publishes a third-party OAuth program.

### D2. `api` is a typed wire protocol; `openai` keeps its legacy spelling

`ProviderEntry.api` now parses to `Protocol::{OpenAiCompletions, OpenAiResponses, AnthropicMessages}`. Gateways may declare any of the three, so a personal gateway that fronts Anthropic or the Responses API registers like any other. The reserved `openai` id still reads `api: "openai-completions"` from existing `models.json` files and still means browser-login Responses; rewriting users' files was out of scope. Catalog refresh is per protocol: `openai-completions` keeps the gateway-specific `/agent/models.json` convention, the other two use the provider's standard `<baseUrl>/models`.

### D3. The builtin tool loop runs for every transport, including browser-login OpenAI Responses

`docs/TOOL_CALLS.md` said Responses stops at `tool execution is disabled until an authorization policy and sandbox are implemented`. The policy (`ahi.actions.yaml`, `ask` prompts, session grants) exists and the in-process path checks are the same for every transport, so the gate was the same for gateways. The loop was lifted out of the gateway branch into `HttpBackend::complete`, and each protocol supplies three things: request body, stream reader, and tool definitions. Responses replays `function_call` and `function_call_output` items; Anthropic replays `tool_use` and `tool_result` blocks. Native replay: with `store: false`, OpenAI's reasoning models require the `reasoning` item that preceded a `function_call` to be sent back with it. The request now asks for `reasoning.encrypted_content`, the parser keeps the terminal output items verbatim, and a fully answered batch is replayed as those items (`NativeReplay`, see D12). This follows the published contract; it has not been exercised against a live reasoning model, which is the first thing to do with a real credential.

### D4. Codex (GPT-6-Astra) reviews; it does not edit

Two agents editing one checkout at once was judged riskier than the time it saves. Codex ran read-only twice: once on the original tree, once on the finished diff. Its findings and where they were or were not adopted are in [Codex review](#codex-review).

### D5. Implicit `anthropic` provider only appears when configured

OpenAI always shows an implicit picker group because its login lives inside ahi. The `anthropic` group appears when `ANTHROPIC_API_KEY` is set in the environment or a `providers.anthropic` entry exists, so unconfigured installs are unchanged and existing tests that index `providers[1]` keep their meaning.

### D6. Anthropic request budget and thinking

`max_tokens` is mandatory for Anthropic. It is `min(4096, maxTokens)` like gateways. Thinking is sent as `thinking: {type: "enabled", budget_tokens}` only when metadata says `reasoning: true` and `compat.supportsReasoningEffort: true` and the level is not Off; budgets are minimal 1024, low 2048, medium 4096, high 8192, clamped to `max_tokens - 1024`, and omitted when that leaves less than 1024. Temperature is never sent.

### D7. Typed error kinds, minimal

`Error` gained an `ErrorKind` (`Auth`, `Policy`, `Transport`, `Protocol`, `Limit`, `Other`). The auth, transport, protocol, limit, and policy paths in the harness and backend set it. `ResponseEvent::Failed` still carries only the message text, so the chat screen does not yet act on the kind; a first attempt to append a `/login` hint was dropped because it would have misdirected gateway users. Carrying the kind to the screen and deciding retries by kind is future work.

### D8. Transcript layout is incremental, not cached

`ChatScreen` keeps a `TranscriptLayout` (rows plus per-message row starts) and a `dirty_from` message index. Any mutation marks the index; the next read re-lays only messages from that index. Visible `Line`s are built for the viewport only. This is a data-layout change, not a cache: there is one source of truth for rows and it is updated at mutation time.

### D9. The last-good catalog files stay

"Delete all caches" was read as direction, not as an instruction to remove `<state>/catalogs/*.json`. Those files are what makes `ahi tui` work offline. The defect was reading them once per model per load; they are now read once per provider.

### D10. Browser-token Responses keeps its restricted request profile

Pi omits `max_output_tokens` and sampling fields for Sign in with ChatGPT. That restriction is now a property of the credential (`CompletionConnection::browser_token`), not of the Responses protocol, so a gateway speaking Responses receives the output budget. The two protocol-neutral limits (`REQUEST_CAP`, `inputLimits.maxRequestBytes`) apply to every protocol before any network use.

### D12. Provider-native blocks ride with the tool calls that need them

Both APIs require a tool-calling assistant turn to be continued with the provider's own opaque output: Anthropic wants the signed `thinking`/`redacted_thinking` blocks in front of the `tool_use` blocks; OpenAI Responses with `store: false` wants the `reasoning` item (with `encrypted_content`) in front of the `function_call`. `ToolPayload::Calls` therefore carries an optional `NativeReplay { protocol, items }`, captured at the terminal event and replayed verbatim, in order, by the same protocol only. It is never shown, never parsed beyond size checks (1 MiB of Responses items, 256 KiB of thinking), and dropped when the transcript is sent to another protocol. `include: ["reasoning.encrypted_content"]` is sent only when reasoning effort is sent, which is Pi's condition as well (`buildParams` in `packages/ai/src/api/openai-responses.ts`, read 2026-11-04); `isChatGPTSignIn` does not omit it. A non-reasoning text turn therefore sends the same body as before this branch. A first version of this branch instead disabled thinking whenever tools were advertised; Codex's second pass argued, correctly, that replaying the blocks is the contract and gating only hides it.

### D13. Draining a finished stream never blocks the turn

ureq returns a connection to its pool only when the body was read to its end. Draining inline risked waiting on a gateway that leaves the body open until the 120-second deadline, so the bounded drain runs on a detached thread. Connection reuse is best effort; the turn continues at the terminal event, as before this branch.

### D14. Anthropic thinking mode is model metadata

Newer Anthropic models reject `budget_tokens` and take `thinking: {type: "adaptive"}` with `output_config.effort`. The typed `compat` block gained `thinkingMode: "budget" | "adaptive"` (default budget). Pi keeps a per-model table for this; ahi keeps it in the same place as every other capability flag, the gateway catalog or the curated entry.

### D15. Tests do not read `ANTHROPIC_API_KEY`

The implicit `anthropic` provider is gated on that variable in the binary. In test builds the check reads a per-thread flag that a test sets explicitly, so a developer who exports the key does not shift provider indexes under unrelated fixtures. The PTY smoke script removes the variable from the child environment for the same reason.

### D11. Session grants bind to the destination URL

Codex pointed out that a grant keyed on the gateway id survives reconfiguring that id to a new `baseUrl`. Grants now also record the resolved inference URL; a moved gateway asks again. The grant test fixture was rewritten to serve every turn from one loopback destination, with an explicit moved-destination case.

### D16. Provider switch posts a notice, not a prompt

Ian's call (2026-11-05): when `/model` lands on a different provider and the transcript has messages, the screen posts one `Notice` row naming the old provider, the new provider, and the count of messages that will go to the new provider with the next message. It is not a confirmation dialog, because the operator has just chosen the provider on purpose and nothing is sent until they type. Switching models within one provider posts nothing. `switching_provider_with_history_posts_a_disclosure_notice_once` covers it.

### D17. Claude subscriptions: delegate to the installed `claude` CLI, as a transport variant

Ian's call (2026-11-05): keep the API-key path (D1) and add delegation to Claude Code in headless mode as another `CompletionTransport` variant. The reference design is Pi's `claude-code-pi` package (https://pi.dev/packages/claude-code-pi), read 2026-11-05. Its "Anthropic terms of service analysis" (last reviewed by its authors 2026-10-20 against the Consumer Terms, Commercial Terms, the Claude Code legal page, the Usage Policy update effective 2026-11-12, and the support article "Use the Claude Agent SDK with your Claude plan") concludes that `claude -p` under the user's own sign-in is explicitly permitted and draws on the plan's limits; that the binary must run unmodified with its sign-in untouched and no OAuth token may be read, stored, or refreshed by the wrapper; that routing other people's requests through one subscription, or hosting it as a service, is intermediation Anthropic enforces against; and that heavy unattended automation is better run on an API key or the API credits included with Max and Team. The verdict there is "safe for personal use with your own credentials". That is the line ahi will hold: one local user, one local `claude`, no token handling, no fallback to direct API calls.

Mechanism, from the same page and the local `claude --help` (2.1.318): one `claude -p` process per turn with `--model`, `--effort <level>` for thinking (off sends no flag), `--no-session-persistence`, `--permission-mode dontAsk`, `--tools ""`, `--strict-mcp-config`, `--disable-slash-commands`, and `--output-format stream-json --include-partial-messages` for token streaming. Pi serializes the whole transcript into the prompt each turn and bridges its tools through `<pi_tool_call>` text markers because Claude Code's own tools are off. ahi has a better option available: host its policy-gated tools as a stdio MCP server passed with `--mcp-config` and route approvals through `--permission-prompt-tool`, so tool calls stay native and `ahi.actions.yaml` stays the only policy. Usage and cost come from the `result` record. The adapter is a process transport, not a wire protocol: nothing under `src/chat/backend/protocol/` changes.

**Exploration, not scheduled (2026-11-05).** What an MCP server would cost, against the prompt bridge:

- *Tokens and money*: a wash. Both put the two tool schemas in front of the model each turn (a few hundred tokens). MCP is cheaper on tool-heavy turns because the call/result loop runs inside one `claude -p` process with Claude Code's own cache breakpoints; the prompt bridge starts a fresh process per tool round and re-sends the serialized transcript. No direct charge either way.
- *Latency*: an MCP handshake is tens to a couple hundred milliseconds per turn, on top of the 1–2 s binary startup that both paths pay. Not the bottleneck.
- *Engineering*: this is the real cost. Stdio MCP makes Claude Code spawn the server as its own child, so a second ahi process would need IPC back to the TUI for approvals and grants. Streamable HTTP MCP avoids that: the TUI listens on `127.0.0.1` (the OAuth callback listener on 1455 is the existing pattern) and passes the URL in `--mcp-config`, so tools run in the TUI process with the existing policy, approvals, and grant state. Approvals need no `--permission-prompt-tool`: allowlist `mcp__ahi__*` and let the tool handler block on the existing approval prompt before it returns. The 8-round cap becomes `--max-turns`; the 256 KiB budget stays in ahi. Size: the prompt bridge is a few hundred lines and proven in Pi; the MCP path is two to three times that plus a JSON-RPC handler (`initialize`, `tools/list`, `tools/call`) and a test server, and it keeps tool calls structured instead of parsed out of text, which Pi flags as the weak point of its own approach.
- *Surface*: one more localhost socket while a turn is active, bound to loopback with a per-session token in the URL so no other local process can drive the tools.

Recommendation when the branch starts: HTTP-transport MCP in the TUI process, no permission-prompt tool.

### D18. Where the time goes, measured (2026-11-05)

Ian asked for paths that make the app 10× faster. Release-build probes on this machine (M-series, 120×40 terminal), then removed from the tree:

| Path | Measured | Verdict |
| --- | --- | --- |
| Idle frame, 300 messages | 180 µs per draw, 0.36 % of a core at 20 Hz | Not a bottleneck. Left alone: draw-on-change would cut idle wakeups, not speed. |
| `load_registry` | 68 µs | Not a bottleneck. |
| Binary start to `--help` | under 10 ms warm | Not a bottleneck. |
| `visible_lines` for a 50-row window | 6 µs | Not a bottleneck. |
| Re-wrap of a streamed 32 KiB message | 205 µs per delta averaged over the stream, 410 µs for the final delta, 33 MB re-segmented in total | Quadratic in message length. Fixed: 1.9 µs per delta, bounded by the delta, not the message. Deltas are batched per frame, so the per-frame saving is one full message layout (0.4–0.7 ms for 32 KiB). |
| `git status --porcelain --untracked-files=all` | 8.2 ms per call here, unbounded on a large tree, on the UI thread at load, on every Enter, and once per drained tool batch | The only stall a user can feel. Fixed: probed on a worker thread, one in flight, footer keeps its last snapshot. |
| `refresh_all` | one provider after another, up to 15 s each | Fixed: one scoped thread per provider; each writes only its own catalog file. N providers now cost the slowest one. |

The wall clock of a turn is the model and the network. Nothing in the UI loop or the request path is within an order of magnitude of either, so there is no local 10× left to take without changing what is sent. The one lever that would change what is sent is provider-side prompt caching (Anthropic `cache_control` breakpoints; OpenAI applies it automatically). It is a request flag, not an application cache, and it cuts input cost by about 90 % and time to first token on tool rounds and long chats, but cache writes cost 25 % more and expire after five minutes, so an idle chat can pay more. Not implemented; Ian's call. `ErrorKind` to the screen stays open: no screen behaviour yet justifies carrying it (D7). Cancellation of blocked reads stays open as a design change.

### D19. Esc cancels a response; the partial reply stays

Ian's calls (2026-11-05): Esc cancels, Ctrl+C keeps clearing the draft, the partial reply is kept as other harnesses do, and a one-line highlighted notice records it. Built: `CompletionBackend::cancel`, an `Arc<AtomicBool>` per turn in `HttpBackend`, a `CancelReader` around the response body that refuses to read once cancelled and discards bytes that arrived after the cancel, checkpoints before each request and each tool round, and a `TranscriptRole::Alert` row (amber background) that failures use as well. The screen drops the receiver at once. Enter still preserves a command draft during a turn, so Esc is the only key that ends one. An assistant message that never received text is removed so Anthropic never sees an empty turn.

What was examined and not done: interrupting a read that is blocked on a silent server. ureq 2.12 ignores `timeout_read` whenever the overall `timeout` deadline is set (`stream.rs` sets the socket timeout to the time until the deadline), so a per-read timeout would mean dropping the 120-second deadline and re-implementing it, and a short socket timeout also applies to the wait for response headers, which Ollama-style servers delay until prompt evaluation finishes. The other route is a custom `TlsConnector` that keeps a cloned `TcpStream` per connection and calls `shutdown` from the UI thread; it does not cover plain-HTTP gateways. Neither saves tokens: a server that is silent is not writing, and it cannot notice a closed socket until it writes, so closing at the next byte is equivalent for billing. The worker therefore lingers until the next byte or the deadline, holding one pooled connection, and nothing it receives reaches the screen. Revisit only if a provider is shown to poll the socket for disconnects during silence.

### D20. `ErrorKind` reaches the screen with the behaviours that need it

Ian's call (2026-11-05, revised the same day to land in this PR): `Failed { message, kind }` plus `Reconnecting { attempt, attempts }`. Built with three behaviours, all specified in `docs/PROVIDERS.md` under Errors:

- *Transport*: three connection attempts with 250 ms and 750 ms pauses, only for ureq `Dns`, `ConnectionFailed`, and `ProxyConnect` failures, which provably never reached the server. HTTP statuses (including 502/503 from a proxy, a judgment call recorded here as not retried), timeouts, and mid-stream failures are reported once, so "generation is never retried" stays true. The backoff checks the cancel flag every 50 ms (D19). Gateways only; the browser-login path keeps its own single renewal. On the screen, a `Transport` failure that leaves the transcript ending in an operator message or tool results (the shape test, not a per-turn flag) makes blank Enter resend; a partial reply at the tail offers nothing.
- *Auth*: `CompletionConnection::credential` records the source at resolve time (browser login, `$VARIABLE`, the `gateway` marker in `models.json`, none). The hint is appended in the turn loop for gateway transports and names the variable or the file, never a value. OpenAI messages are untouched.
- *Limit*: the two request-cap messages name the size and the bound and point at `/clear` or a shorter message; the tool round limit is now a `Limit` as the docs already claimed.

## Codex review

GPT-6-Astra ran twice through the Codex CLI, read-only (`codex exec -m gpt-6-astra -s read-only`), with the same "work to completion, state assumptions instead of asking" guidance the owner gave the implementer. The first prompt briefed it with file paths and three questions, not with conclusions, and asked it to disagree with the docs where warranted.

### Pass 1: the original tree

Adopted, beyond the implementer's own list:

- **Mixed-policy cancellation** (`tool_loop.rs`): a dropped reply left rule-approved calls in the same batch runnable. Now every undecided and approved call in a cancelled batch is denied with `approval cancelled`; `a_cancelled_approval_runs_no_call_in_a_mixed_batch` covers it.
- **256 KiB was a threshold, not a cap**: only successful content was charged, after the fact. Every outcome is now charged and over-budget content is cut; `the_turn_output_bound_is_a_cap_including_diagnostics` covers it.
- **Two synchronous `git status` runs per tool call** on the UI thread: `ToolResult` no longer refreshes; `ToolCallReturned` marks the location dirty and `poll_backend` refreshes once per drained batch.
- **ureq closes connections with unread bytes**, so a retained agent alone would not have restored pooling: the SSE reader drains a bounded remainder after a terminal event.
- **Grants keyed on the gateway string** (D11).
- **Protocol extensibility blocked in configuration and discovery, not just dispatch**: `api` is parsed into `Protocol` at load, catalogs carry it, registration saves it, the legacy `openai` spelling is handled once in `check_entry`/`completion_connection`.
- **Browser-token restrictions are a credential profile, not a Responses property** (D10).
- **Request ownership**: `submit` moves the request into the worker; the loop appends tool exchanges to that history instead of cloning it again.
- **Draw wrapped the transcript twice** (`clamp_scroll` then `wrapped_lines`): both now read one synced layout.

Noted, not done in this branch:

- A `CompletionHandle` with real cancellation of blocked HTTP reads. Agreed that an atomic flag checked between reads is insufficient; this needs a socket-level abort and is left as the next item.
- A full `TurnConfiguration` snapshot and an explicit history-disclosure policy on provider switch. The transcript still follows the operator across `/model`; disclosure of earlier tool results to a different provider is a product decision for the owner.
- Carrying `ErrorKind` to the screen (D7).
- Codex's assessment that the `AssistantTurn` proposal in `docs/TOOL_CALLS.md` permits contradictory states (failed termination plus executable requests) and should be a validated completed-turn type. Recorded for the roadmap; the proposal text was not rewritten.

Codex also corrected the brief: configuration and policy load once per turn, not per tool round; replay keeps matched calls rather than dropping whole batches, so the docs' "incomplete exchanges are dropped" was too broad (now reworded); and the roadmap's claim that only an echo backend exists is stale.

### Pass 2: the finished branch

Verdict was "ship after fixing items 1–6"; all six were fixed on the branch, then the suite, Clippy, rustdoc and the PTY workflow were rerun.

| # | Finding | Outcome |
| --- | --- | --- |
| 1 | Anthropic tool continuation dropped thinking blocks and signatures | Fixed: captured and replayed (D12) |
| 2 | Responses replay dropped `reasoning` items required with `store: false` | Fixed: `include: reasoning.encrypted_content`, native output items replayed (D12) |
| 3 | Budget-style `thinking` is rejected by newer models that need adaptive thinking | Fixed: `compat.thinkingMode` (D14) |
| 4 | Parser accepted content events after `message_delta`, so a late `tool_use` block could reach execution | Fixed: `message_delta` requires every block closed and closes content; later block events fail. `pause_turn` is now an unsupported stop reason. |
| 5 | `/models` refresh replaced discovery metadata with names only | Fixed: previous typed metadata for still-listed ids is kept; the name overlays it |
| 6 | Inline drain could block a finished turn until the HTTP deadline | Fixed: detached bounded drain (D13) |
| 7 | `request.clone()` still duplicated the transcript per turn | Fixed: `messages` is moved out of the request |
| 8 | Materializing an implicit provider reordered saved providers | Fixed: inserted at the front, saved positions untouched |
| 9 | A long streaming message is re-wrapped from its start on every delta while scrolled | Fixed later on the branch (D18): an append to the last message resumes from its last row. |
| 10 | Doc wording: headers cannot override auth headers; 64 KiB is per call; stale `http/responses.rs` link | Fixed |

Codex also confirmed: every validated call gets exactly one result; cancelled batches run nothing; grants carry the destination; raw arguments stay out of diagnostics; no route exists from the browser credential to a gateway; no production mutation of the transcript misses a dirty mark. Still open from both passes: real cancellation of blocked reads, a turn-configuration snapshot (the disclosure notice is D16), `ErrorKind` on the screen, and the roadmap's `AssistantTurn` type. Item 9 was fixed afterwards (D18).

## Review answers (2026-11-05)

Ian reviewed the finished branch and answered the three questions held for the end.

- **D1 stands; the API-key path stays.** The follow-up is a different mechanism, not an OAuth transport: delegate a Claude provider to the installed `claude` CLI in headless mode (`claude -p`, Claude Code 2.1.318 verified locally). It runs Anthropic's own client under the user's own login, so nothing is impersonated; the model and thinking come from `--model` and `--effort <low|medium|high|xhigh|max>`; token-level streaming from `--output-format stream-json --include-partial-messages`; `--tools ""` plus `--strict-mcp-config` keeps Claude Code's own tools out so ahi's policy-gated tools remain the only tools, exposed to the CLI as an MCP server that ahi hosts, with `--permission-prompt-tool` routing approvals back to the chat screen. Recorded as D17 with Pi's terms-of-service analysis; it belongs on its own branch.
- **Push and open the PR.** Done from this branch.
- **History disclosure on `/model` switch** was explained (it is a disclosure point, not a cost point: the transcript, including tool output from the working tree, goes to whichever provider is active on the next turn, and native replay items are dropped). Ian chose the notice; see D16.
