# Gateway model configuration

Status: implemented and verified locally, including lead reruns. This page records the chosen model, boundaries, and observable proof; it is not a published release. This extends [gateway discovery](GATEWAY_DISCOVERY.md) without replacing [the shared terminal experience](TERMINAL_EXPERIENCE.md).

## Goal and existing failure

A Pi-shaped discovery response has a top-level `providers` map. The former ahi decoder accepts only a flat provider catalog and then reduces each model to its identifier. Consequently a reachable, valid Pi catalog fails before Review, and model capabilities never reach the request builder.

The gateway should publish one deliberately sanitized catalog, not maintain separate model lists for each client. ahi will normalize the existing flat catalog and the supported Pi-shaped subset into the same typed model. Accepting a configuration format does not authorize Pi command execution, arbitrary credentials, tools, or new protocols.

## Design alternatives

| Alternative | Consequence | Decision |
| --- | --- | --- |
| Translate server responses into identifier-only ahi manifests | Minimal client changes, but loses capabilities and duplicates representations | Reject |
| Treat arbitrary remote Pi configuration as local executable configuration | Broad compatibility, but unsafe credential/command/endpoint behavior | Reject |
| Normalize both supported JSON shapes into typed gateway/model metadata | One authoritative model list, explicit boundaries, useful backend capabilities | Selected |

## Four-part delivery checkpoint

- [x] **Blocking first steps:** inspect discovery, cache, local curation, and request construction; choose the model and trust boundaries before editing.
- [x] **Independent workstreams:** one implementation owner handles the coupled Rust/PTY change; the lead owns documentation, review, and independent verification.
- [x] **Shared mutable state:** serialize source changes; use disposable test state, loopback servers, and an isolated build directory. Never alter the user's gateway or live configuration.
- [x] **Smallest safe decomposition:** schema normalization → metadata persistence/resolution → request/UI consumers → observable local workflows. A single implementation owner avoids incompatible intermediate types across these coupled consumers.

## Supported catalog example

The following uses the same field structure as the reported Pi catalog. An entered `https://gateway.example` discovers this document at `/agent/models.json`, then shows `https://gateway.example/v1` in Review:

```json
{
  "providers": {
    "home": {
      "api": "openai-completions",
      "apiKey": "gateway",
      "baseUrl": "https://gateway.example/v1",
      "models": [{
        "id": "GLM-5.3-Flash-EXL3",
        "name": "GLM-5.3 Flash",
        "contextWindow": 1048576,
        "maxTokens": 32768,
        "input": ["text", "image"],
        "inputLimits": {"maxRequestBytes": 33554432},
        "reasoning": false,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "compat": {
          "supportsDeveloperRole": false,
          "supportsReasoningEffort": false,
          "supportsStore": false,
          "supportsUsageInStreaming": true,
          "supportsFinishReason": true,
          "maxTokensField": "max_tokens",
          "requiresToolResultName": false,
          "requiresAssistantAfterToolResult": false,
          "requiresThinkingAsText": false,
          "requiresReasoningContentOnAssistantMessages": false,
          "thinkingMode": "budget"
        }
      }]
    }
  }
}
```

`name` must be nonempty display text without control characters. Capacities are positive integers; modalities and the token-parameter name are closed enums. Cost components are optional finite nonnegative numbers, in USD per million tokens, not an assertion that an unknown price is free. The supported compatibility fields are exactly those shown; unknown model/compatibility fields are rejected. This is a supported Pi subset, not execution of arbitrary `models.json` features such as `!command`, sampling maps, model endpoint overrides, or image resizing.

## Model and persistence contract

Introduce typed `ModelMetadata`, `CompletionCompatibility`, token-limit-field and modality enums, nonnegative finite per-million-token costs, and byte/token capacities. Keep ordered catalog identifiers as the membership/index contract and attach metadata by identifier. This permits existing string-only caches to load with unknown metadata, while new cache writes retain the supported fields. Do not invent a capacity, price, or capability for an old cache.

Supported model fields are `id`, `name`, `contextWindow`, `maxTokens`, `inputLimits.maxRequestBytes`, `input`, `reasoning`, `cost`, and the compatibility fields in the supplied configuration. Provider-level compatibility defaults merge with model overrides field by field. Local curated model metadata, when explicitly present, takes precedence over cached metadata. Curation remains an explicit availability list and must preserve metadata already present in the local document.

An absent value means unknown or the documented legacy transport default, not an inferred model capability. Invalid known fields produce actionable, secret-safe diagnostics. No blanket acceptance of arbitrary executable Pi configuration or unimplemented APIs is intended.

## Discovery and endpoint contract

- Continue requesting `GET <origin>/agent/models.json` without credentials, cookies, or redirects.
- Accept the legacy flat catalog and a `providers` map. Select a unique appropriate provider; ambiguity must be explicit rather than selecting the first map entry. Registered-provider refresh selects by identifier.
- Validate returned inference `baseUrl` using the same URL policy as Connect. A returned same-origin `/v1` path is authoritative for a new registration and visible in Review. Do not silently accept a different origin or redirect existing registered settings.
- Accept `api` as one of `openai-completions`, `openai-responses`, or `anthropic-messages` (see [Providers](PROVIDERS.md)), reserve `openai`, and allow only the `gateway` marker or a valid `$NAME` environment reference. Never evaluate Pi `!command` values, shell interpolation, or literal secrets.
- Preview is read-only. Registration and curation remain separate explicit operations. Existing snapshot conflict checks, atomic file replacement, last-good-cache behavior, and immediate partial-cache warnings remain intact.

## Gateway request and shared UI contract

- Resolve one effective typed model for a turn; consumers must not repeatedly interpret arbitrary JSON.
- Send `stream_options.include_usage` unless explicitly unsupported. If finish reasons are explicitly unsupported, infer completion only at `[DONE]`; retain all existing malformed/truncated/tool-call checks.
- Treat `maxTokens` as the output ceiling, not an instruction to request the entire context window. With a declared ceiling, use a conservative named default output budget of 4096 tokens, capped by that ceiling, and use the declared `max_tokens`/`max_completion_tokens` field. Legacy models without a ceiling retain their prior request behavior. Without an authoritative tokenizer, do not claim exact context-window enforcement or invent prompt-token counts.
- Enforce a declared `maxRequestBytes` against the actual serialized UTF-8 request before network submission.
- Apply reasoning effort only when both model reasoning and compatibility support explicitly permit it. Do not send the literal `off` as an OpenAI effort value. Preserve store/developer/tool/thinking-message compatibility flags without inventing workflows that do not exist. If store is sent, it must be false and only when supported.
- Use reported token usage and complete pricing information for cost calculations; keep unknown costs unknown. Cached input must not be double-counted. Prices use USD per million tokens, following Pi's documented units.
- Show model names and useful capability/limit metadata in selection/catalog UI. Known context capacity reaches chat's existing context meter. Image metadata does not enable image attachment. Tool metadata does not grant execution: gateway builtin tools require local `ahi.tools.yaml` approval, and OpenAI Responses tools remain disabled.

The separately implemented [OpenAI browser-login transport](OPENAI_INFERENCE.md) uses Responses rather than these Chat Completions wire fields. It retains shared metadata/byte limits/accounting but omits browser-token-unsupported output-limit fields, following the inspected Pi adapter.

## Acceptance evidence

Extend the existing `scripts/smoke-model-picker.py` rather than introducing a parallel runner or new unit tests. Use real terminal interaction and a loopback server to prove: the supplied configuration shape reaches Review; returned `/v1` is shown and used; metadata survives cache reload and curation; compatibility changes the actual HTTP request/stream handling; byte-limit rejection produces no HTTP request; old caches and flat catalogs still work; ambiguous/cross-origin/invalid metadata failures preserve prior state; all existing onboarding/chat checks remain intact.

No live inference requests, credential changes, installations, commits, pushes, or publication are authorized. A successful local fixture does not prove live gateway inference or full Pi compatibility.

## Implementation and verification record

[`src/harness/metadata.rs`](../src/harness/metadata.rs) owns the typed model and overlays; [`src/harness/mod.rs`](../src/harness/mod.rs) owns normalization, provider selection, caching and effective metadata resolution. [`src/harness/gateway.rs`](../src/harness/gateway.rs) retains the existing registration guards. [`src/harness/curation.rs`](../src/harness/curation.rs) preserves rich local model entries rather than reconstructing them as IDs on each save. [`src/chat/backend/http.rs`](../src/chat/backend/http.rs) consumes the resolved metadata, and [`src/tui/mod.rs`](../src/tui/mod.rs) shows the selected model's detail summary.

The cache retains ordered `models` for membership and a `metadata` map for lookup. This is an index plus attached details, not a second model-selection list. An unavailable or old catalog does not populate curated models. Explicit local values overlay cached values field by field, including individual compatibility and price components.

Developer/thinking-block workflows remain dormant. Gateway builtin replay honors `requiresToolResultName` and `requiresAssistantAfterToolResult` when serializing complete assistant-call/result exchanges; metadata alone never enables the tools. Assistant text replay can supply an explicitly required empty `reasoning_content`; no reasoning text/signature is invented. Off omits effort rather than promising the provider disables its own default reasoning.

For cost, cached input is subtracted from ordinary input before applying its rate. Missing cached counts leave cost unknown unless input and cache-read rates are equal; missing necessary prices or inconsistent cached counts also leave it unknown. The displayed Commands figure is cumulative reported-usage cost, not a billing total. Cache-write pricing is retained but no cache-write usage is fabricated.

### Reproduce verification

The final combined source also includes [OpenAI Responses](OPENAI_INFERENCE.md); use its complete [verification commands](OPENAI_INFERENCE.md#safe-local-verification-and-troubleshooting). The metadata implementation was first verified separately under `target/model-configuration-verification` and then rerun with the combined source under `target/openai-inference-verification`. Formatting, Clippy, all 167 existing Rust tests, isolated builds, the full PTY workflow, rustdoc, and offline package verification passed. No new unit tests or paid/external inference calls were added.

The maintained PTY runner proves the supplied two-model Pi shape using a loopback origin: discovery from origin, authoritative `/v1` Review/save/request, explicit Space/Enter, streamed response and follow-up, metadata cache/restart/curation, local/provider overrides, both token fields and ceiling caps, reasoning/store/usage behavior, terminal-only finish inference, malformed tools/truncation, exact UTF-8 byte-limit acceptance and rejection with zero rejected-body requests, decimal pricing and unknown cached usage, provider ambiguity/exact/registered selection, invalid metadata/cross-origin retention, and old cache/flat-catalog behavior.

Optional captures beneath the chosen directory include `metadata-supplied-review.txt`, `metadata-catalog-details.txt`, `metadata-context-meter.txt`, `metadata-exact-utf8-limit.txt`, `metadata-byte-rejection.txt`, `metadata-paid-usage.txt`, and `metadata-unknown-cached-usage.txt`, with paired ANSI streams. Fixtures establish actual local behavior, not successful live gateway or OpenAI inference.

### Limits and recovery

- **Ambiguous provider:** enter the exact declared inference base URL. If several providers still share it, the server must disambiguate its discovery catalog; ahi never chooses a map's first entry.
- **Unsupported/invalid metadata:** correct the named field on the published catalog, then rediscover/refresh. Failed refresh keeps the last-good catalog and curation unchanged.
- **Changed registered URL:** review local configuration explicitly; rediscovery never silently redirects an existing provider.
- **Body exceeds `maxRequestBytes`:** shorten/clear history or choose a model with a suitable limit. The rejected body is not submitted.
- **Unknown capacity/cost:** inspect supplied metadata and reported usage. ahi does not derive a context window from a model name or invent cache counts.
- **Image/tool flags:** metadata is descriptive; image submission remains disabled. Gateway builtin tool execution requires explicit local policy, not catalog capabilities; OpenAI execution remains disabled. See [Tool calls](TOOL_CALLS.md) for the implemented subset and future roadmap.
