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

`docs/TOOL_CALLS.md` said Responses stops at `tool execution is disabled until an authorization policy and sandbox are implemented`. The policy (`ahi.actions.yaml`, `ask` prompts, session grants) exists and the in-process path checks are the same for every transport, so the gate was the same for gateways. The loop was lifted out of the gateway branch into `HttpBackend::complete`, and each protocol supplies three things: request body, stream reader, and tool definitions. Responses replays `function_call` and `function_call_output` items; Anthropic replays `tool_use` and `tool_result` blocks. Not done: native reasoning-item replay for Responses (text and tool exchanges only). A live check against a reasoning model with `store: false` is still owed.

### D4. Codex (GPT-6-Astra) reviews; it does not edit

Two agents editing one checkout at once was judged riskier than the time it saves. Codex ran read-only twice: once on the original tree, once on the finished diff. Its findings and where they were or were not adopted are in [Codex review](#codex-review).

### D5. Implicit `anthropic` provider only appears when configured

OpenAI always shows an implicit picker group because its login lives inside ahi. The `anthropic` group appears when `ANTHROPIC_API_KEY` is set in the environment or a `providers.anthropic` entry exists, so unconfigured installs are unchanged and existing tests that index `providers[1]` keep their meaning.

### D6. Anthropic request budget and thinking

`max_tokens` is mandatory for Anthropic. It is `min(4096, maxTokens)` like gateways. Thinking is sent as `thinking: {type: "enabled", budget_tokens}` only when metadata says `reasoning: true` and `compat.supportsReasoningEffort: true` and the level is not Off; budgets are minimal 1024, low 2048, medium 4096, high 8192, clamped to `max_tokens - 1024`, and omitted when that leaves less than 1024. Temperature is never sent.

### D7. Typed error kinds, minimal

`Error` gained an `ErrorKind` (`Auth`, `Policy`, `Transport`, `Protocol`, `Limit`, `Other`). Only the auth paths set a kind today; the chat screen uses it to append a `/login` hint. Full retry policy by kind is future work.

### D8. Transcript layout is incremental, not cached

`ChatScreen` keeps a `TranscriptLayout` (rows plus per-message row starts) and a `dirty_from` message index. Any mutation marks the index; the next read re-lays only messages from that index. Visible `Line`s are built for the viewport only. This is a data-layout change, not a cache: there is one source of truth for rows and it is updated at mutation time.

### D9. The last-good catalog files stay

"Delete all caches" was read as direction, not as an instruction to remove `<state>/catalogs/*.json`. Those files are what makes `ahi tui` work offline. The defect was reading them once per model per load; they are now read once per provider.

## Codex review

Filled in after each Codex pass. See below.
