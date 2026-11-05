# Providers: identity, protocol, and authentication

Three independent choices describe how ahi talks to a model. They used to be fused in the `openai` special case; they are now separate fields and types so a new combination is configuration, not code.

| Axis | Values | Where it lives |
| --- | --- | --- |
| **Identity** | the `providers.<id>` key in `models.json`; `openai` and `anthropic` are built-in ids | the operator's selection, `<id>/<model>` |
| **Wire protocol** (`api`) | `openai-completions`, `openai-responses`, `anthropic-messages` | `harness::Protocol`; one adapter each under `src/chat/backend/protocol/` |
| **Authentication** | `apiKey: "gateway"` marker, `apiKey: "$NAME"` environment reference, extra `headers`, or the built-in OpenAI browser login | `harness::CompletionTransport`; secrets are resolved only when a turn is submitted |

A protocol never implies an authentication scheme, and a configured gateway can never become the destination for the browser credential.

## Protocols

| `api` | Request | Streaming grammar | Tool definitions | Tool replay |
| --- | --- | --- | --- | --- |
| `openai-completions` | `POST <baseUrl>/chat/completions` | `choices[0].delta`, `[DONE]` | `tools[].function` | assistant `tool_calls` + `role: tool` |
| `openai-responses` | `POST <baseUrl>/responses` | `response.*` output items, `response.completed` | `tools[]` with `name`/`parameters` | `function_call` + `function_call_output` input items |
| `anthropic-messages` | `POST <baseUrl>/messages` | `message_start` … `message_stop` content blocks | `tools[]` with `input_schema` | assistant `tool_use` blocks + user `tool_result` blocks |

Every protocol feeds the same turn loop in `src/chat/backend/http.rs`: serialize, send, decode into one `StreamOutcome`, run approved builtin tools, continue. Bounds are shared: 1 MiB per SSE event, 16 MiB per stream, 32 calls per batch and 64 KiB of argument text per call, 8 rounds and 256 KiB of tool content per turn (plus one short truncation notice per call), and `inputLimits.maxRequestBytes` on every request.

**Native replay.** When a response ends in tool calls, the protocol keeps the provider's own output alongside the validated calls (`NativeReplay`): Responses keeps the terminal output items, including `reasoning` items with `encrypted_content`; Anthropic keeps `thinking` and `redacted_thinking` blocks with their signatures. The continuation request replays them verbatim, in order, to the protocol that produced them, which is what both APIs require for reasoning models with `store: false` and for extended thinking with tool use. They are opaque: never displayed, never interpreted, dropped if the transcript is later sent to a different protocol, and bounded (1 MiB of items, 256 KiB of thinking).

Protocol-specific request behavior:

- **Chat Completions** honors the typed `compat` flags: `maxTokensField`, `supportsUsageInStreaming`, `supportsStore`, `supportsFinishReason`, `requiresToolResultName`, `requiresAssistantAfterTooLResult`, `requiresReasoningContentOnAssistantMessages`. Reasoning effort is sent only when `reasoning: true` and `compat.supportsReasoningEffort: true`.
- **Responses** sends `store: false`, `include: ["reasoning.encrypted_content"]`, and text history plus complete tool exchanges; a fully answered batch is replayed as its native output items (reasoning, message, function calls) followed by `function_call_output` items. `max_output_tokens` is sent for gateways and omitted for the browser-login credential, which rejects it. Not yet verified against a live reasoning model; the request shape follows the published contract.
- **Anthropic Messages** always sends `max_tokens` (`min(4096, maxTokens)`, default 4096). Extended thinking is sent when `reasoning: true` and `compat.supportsReasoningEffort: true`: with `compat.thinkingMode: "adaptive"` as `thinking: {type: "adaptive"}` plus `output_config.effort` (low/medium/high), otherwise as `thinking: {type: "enabled", budget_tokens}` with budgets minimal 1024, low 2048, medium 4096, high 8192, clamped to `max_tokens - 1024` and omitted below 1024. Newer models require adaptive mode; declare it in the model's `compat`. Thinking blocks are kept for replay (see native replay above), never displayed. Usage input is reported as uncached plus cache-read plus cache-creation tokens, with cache reads as the cached subset; cache-creation tokens are priced at the input rate, which understates Anthropic's write premium. `pause_turn` is not supported (no server tools are advertised). Temperature is never sent. No system prompt, user agent, or identity header is added.

## Authentication

| Provider | How | Notes |
| --- | --- | --- |
| `openai` (built-in) | Sign in with ChatGPT, `ahi login` or `/login` | Browser-login credential, Responses only, destination pinned to `https://api.openai.com/v1/responses`. Any `apiKey`, `headers`, non-public `baseUrl`, or `api` other than the legacy `openai-completions` spelling is a configuration error. Register proxies as separate gateways. |
| `anthropic` (built-in) | `ANTHROPIC_API_KEY` in the environment | Appears in the picker when the variable is set or a `providers.anthropic` entry exists. Default entry: `baseUrl: https://api.anthropic.com/v1`, `api: anthropic-messages`, `apiKey: $ANTHROPIC_API_KEY`. The key is sent as `x-api-key` with `anthropic-version: 2023-06-01`. Catalog refresh is `GET /v1/models`, following up to 8 pages. |
| any gateway | `apiKey` marker or `$NAME`, plus `headers` | `openai-*` protocols send `Authorization: Bearer <value>`; `anthropic-messages` sends `x-api-key: <value>` and `anthropic-version`. `headers` may add further headers; a name that collides with the authentication, version, or reserved transport headers is a configuration error, not an override. |

Why there is no Claude subscription (Pro/Max) login: the only known client path requires presenting ahi as Claude Code (its user agent, `x-app` header, beta flags, scope, and a mandatory "You are Claude Code" system block). ahi does not impersonate another client. The `CompletionTransport` enum is the seam for an officially supported grant: one more variant, one credential module modeled on `src/harness/openai.rs`, and no protocol changes. See `docs/IMPROVEMENT_PLAN.md`, decision D1.

## Catalog refresh by protocol

| `api` | Explicit refresh (`r`, `ahi tui --refresh`) |
| --- | --- |
| `openai-completions` | the gateway convention `GET <origin>/agent/models.json`, no credentials, as before |
| `openai-responses`, `anthropic-messages` | the provider's own `GET <baseUrl>/models` with the configured authentication; `data[].id` and, when present, `display_name`. Typed metadata that `/gateway` discovery or an earlier refresh recorded for an id that is still listed is kept; only the name is refreshed. |
| `openai` built-in | `GET https://api.openai.com/v1/models` with the browser credential, as before |

Discovery through `/gateway` still fetches `/agent/models.json`; a catalog may now declare any of the three protocols, and registration saves the declared value.

## Configuration examples

```json
{
  "providers": {
    "anthropic": {
      "baseUrl": "https://api.anthropic.com/v1",
      "api": "anthropic-messages",
      "apiKey": "$ANTHROPIC_API_KEY",
      "models": [{"id": "claude-sonnet-4-5", "maxTokens": 8192, "reasoning": true,
                  "compat": {"supportsReasoningEffort": true}}]
    },
    "proxy": {
      "baseUrl": "https://gateway.example/v1",
      "api": "openai-responses",
      "apiKey": "$PROXY_KEY",
      "models": [{"id": "gpt-5", "maxTokens": 4096}]
    },
    "home": {
      "baseUrl": "http://127.0.0.1:11434/v1",
      "api": "openai-completions",
      "apiKey": "gateway",
      "models": [{"id": "qwen3"}]
    }
  }
}
```

## Switching providers mid-conversation

`/model` keeps the transcript. If the new selection is on a different provider and the conversation already has messages, the screen posts a notice before anything is sent:

```
switched provider from home to anthropic: the 12 messages above will be sent to anthropic with your next message
```

The notice is informational, not a prompt: the operator chose the provider deliberately, and nothing leaves the machine until the next message. Switching models within one provider posts nothing. Provider-native replay items (Responses reasoning items, Anthropic thinking blocks) are dropped when the history reaches another protocol.

## Errors

`ahi::Error` carries an `ErrorKind` (`Auth`, `Policy`, `Transport`, `Protocol`, `Limit`, `Other`). Authentication failures on every path are `Auth`; policy denials and the disabled-tools boundary are `Policy`; byte, count, and round limits are `Limit`. The chat screen still shows the message text as a highlighted alert; the kind is not yet carried to it.

**Planned use of the kind (decided 2026-11-05, not yet built).** The kind reaches the screen together with the first behaviour that needs it, and three are wanted:

| Kind | Screen behaviour |
| --- | --- |
| `Transport` | Reconnection attempts when a gateway cannot be reached: bounded retries with backoff before the request is sent, never after content has streamed, and an `Enter` resubmits the same message without retyping. |
| `Auth` | A next-step hint that depends on the connection's credential scheme, derived where the connection is resolved (browser login, API key, gateway headers), never from the kind alone. |
| `Limit` | Say which bound was hit (request bytes, stream bytes, tool budget, tool rounds) and suggest `/clear` or a smaller message when the request cap is the cause. |

Until then `ResponseEvent::Failed` carries only the message text.
