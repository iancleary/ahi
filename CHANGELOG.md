# Changelog

## Unreleased

### Fixed

- Native Windows clipboard copies now write `CF_UNICODETEXT` through the Win32 API (no process, no temporary file, no registry dependency), normalizing bare LF to CRLF. If the clipboard stays busy, the PowerShell fallback hands the text over through a private UTF-8 temporary file, as WSL already did. The FFI lives in the in-repo, dependency-free `crates/ahi-win32` workspace crate, so `ahi` keeps `unsafe_code = "forbid"`. The clipboard code is split into platform routing, bounded helper execution, OSC 52, and the Windows handoff, and the routing for macOS, native Linux, WSL, Windows, and remote sessions is unit-tested.
- Browser-login OpenAI Responses turns failed with `invalid or conflicting OpenAI response event` as soon as the model finished: the live API now sends `response.completed` with an empty `output` array (and no `Content-Type` header), and the parser required that array to list every streamed item. The finished output items now stand in for an empty terminal `output`, for text and tool-calling turns alike; a terminal event that lists items is still reconciled one for one, and an item still pending still fails.

### Added

- The tool approval prompt lists the answers for the call awaiting a decision. `↑`/`↓` move the highlight and `Enter` applies it; the letter keys (`y`, `s`, `n`, `a`, `Esc`) still answer directly. `Shift+↑`/`Shift+↓` move the cursor to another numbered call, so calls can be answered in any order and a decision revised before the batch is sent; the cursor otherwise continues at the next undecided call. "Approve all remaining" and "Deny all remaining" apply to every undecided call and are offered only while more than one is undecided.
- `ahi tui --actions <file>` uses that tool policy file instead of the working directory's `ahi.actions.yaml`. The file must exist and parse (checked at startup and on every turn); rule paths and tool confinement stay relative to the working directory. `examples/actions` holds six ready-made policies, from read-everything to deny-all plus a deliberately invalid one, with a README.
- `models.json` `api` is a typed wire protocol: `openai-completions`, `openai-responses`, or `anthropic-messages`. Gateways may declare any of the three; `/gateway` discovery accepts and saves the declared value. The reserved `openai` id keeps its legacy `openai-completions` spelling and still means browser-login Responses. See [docs/PROVIDERS.md](docs/PROVIDERS.md).
- Anthropic Messages transport. A built-in `anthropic` provider appears in the picker when `ANTHROPIC_API_KEY` is set (or a `providers.anthropic` entry exists); the key is sent as `x-api-key` with `anthropic-version: 2023-06-01`. Streaming text, `tool_use`/`tool_result` exchanges, extended-thinking budgets, usage with cache reads, and `GET /v1/models` catalog refresh are supported. There is no Claude subscription login; the plan records why.
- Catalog refresh for `openai-responses` and `anthropic-messages` providers uses the provider's standard `GET <baseUrl>/models` with the configured authentication.
- `ahi::Error` carries an `ErrorKind` (`Auth`, `Policy`, `Transport`, `Protocol`, `Limit`, `Other`).
- Native replay: a tool-calling turn keeps the provider's own output and the continuation replays it verbatim to the same protocol. Responses keeps its terminal output items (reasoning items with `encrypted_content`, requested with `include: ["reasoning.encrypted_content"]` whenever reasoning effort is sent); Anthropic keeps `thinking`/`redacted_thinking` blocks with signatures. Never displayed; dropped on a protocol change.
- `ResponseEvent::Failed` carries the `ErrorKind`. A gateway that cannot be reached (DNS or connection failure) gets three connection attempts with 250 ms and 750 ms pauses, shown as `Reconnecting n/3`; nothing the server may have received is retried. After a connection failure, `Enter` on a blank input resends the same transcript. Authentication failures name where the gateway credential is configured (environment variable or `models.json`), never its value. Request-cap failures name the bound and suggest `/clear` or a shorter message; the tool round limit is a `Limit`.
- `Esc` cancels a streaming response when no command menu, selection, or tool approval owns the key. The partial reply stays in the transcript and is sent with the next message; an assistant turn that received no text is removed; a highlighted `Alert` row records the cancellation. The backend stops before its next request, tool round, or read, discards bytes that arrive after the cancel, and closes the connection. Failed turns are shown as highlighted alerts as well.
- Switching to a model on a different provider with `/model` while the transcript has messages posts a notice naming both providers and the number of messages that will be sent to the new one with the next message.
- `compat.thinkingMode` (`budget`, the default, or `adaptive`) selects how an Anthropic model receives extended thinking: `thinking: {type: "enabled", budget_tokens}` or `thinking: {type: "adaptive"}` with `output_config.effort`.

### Changed

- The builtin tool loop is protocol-neutral and runs for every transport, including browser-login OpenAI Responses, behind the same `ahi.actions.yaml` policy. Responses replays `function_call`/`function_call_output` items and, for gateways, sends `max_output_tokens`. Without an approving rule every transport still fails with `tool execution is disabled`.
- Session grants (`s` at an approval prompt) are bound to the gateway's exact destination URL as well as its id and the working directory; reconfiguring a gateway's `baseUrl` requires a new approval.
- A dropped or malformed approval reply cancels the whole batch: calls that rules alone had approved in the same batch are denied with `approval cancelled` instead of running.
- The 256 KiB per-turn tool content bound is a cap. Every result, including denials and errors, is charged; content past the budget is cut and replaced by a fixed notice.
- One HTTP client per chat session reuses connections across tool rounds and turns; streams are drained after their terminal event so pooled connections stay usable.
- The chat transcript is laid out incrementally: only changed messages are re-wrapped, a streamed reply re-wraps only its last row plus the new text, and only the visible window is padded per frame. Git status is refreshed once per drained batch of tool returns rather than twice per call, and it runs on a worker thread so a large tree never stalls a keystroke; the footer keeps its last snapshot until the probe lands.
- Catalog refresh (`r`, `--refresh`) runs every provider concurrently; the wait is the slowest provider, not the sum.
- Provider metadata is merged from one catalog read per provider instead of one read per curated model.

- Chat can run builtin, read-only `read_file` and `list_dir` tool calls for configured gateways. Tools are off unless the working directory's `ahi.actions.yaml` (version 1, `action: tool_call`) has an approving rule; without one, requests are unchanged and calls fail with `tool execution is disabled`. Rules match a builtin tool and an optional path prefix, deny first. `ask: true` prompts in chat: `y` once, `s` for the session under the shown directory, `n` deny, `a` approve the rest, `Esc` deny the rest. Tools are confined to the working directory, follow no symbolic links, never read the state directory, and return bounded output (64 KiB per file, 500 entries, 256 KiB and 8 rounds per turn). Every call returns a result, and tool exchanges replay on later turns. Approved file contents are sent to the selected gateway. ahi does not download or define tools from configuration.

## 0.3.2 (2026-11-04)

### Changed

- Releases now publish three attested archives: `linux-x86_64` and `linux-aarch64` (musl-static, bit-reproducible as before), plus `windows-x86_64` (built natively on `windows-2025`, unsigned and not promised to be bit-reproducible).
- `ahi update` now supports Windows x86_64 without Cargo. Windows moves the running executable aside before replacing it. Linux support is unchanged. macOS archives and CI are deferred while hosted ARM64 runners lack capacity; use `cargo install ahi` on macOS. See `docs/macos-update-follow-up.md` for the restoration checklist.

## 0.3.1 (2026-11-04)

### Added

- `ahi update` replaces the running ahi binary from the attested Linux archive of the latest release. It resolves the latest GitHub release, downloads `ahi-<version>-sha256sums.txt` and the target archive, verifies both files against their GitHub build-provenance attestations with the `gh` CLI, checks the archive sha256, extracts the single `ahi` member, and atomically renames it over the running binary. Any failure refuses the target and leaves the binary untouched. Linux only; macOS and Windows keep `cargo install`. The command is not named `self-update`.

## 0.3.0 (2026-11-04)

### Breaking changes

- `ahi tui` loads only local configuration and catalogs. It no longer fetches gateway catalogs at startup.
- The main picker offers exactly `providers.<id>.models` from the state directory's `models.json`, in saved order. Catalogs never supply picker defaults. A gateway with omitted or empty `models` no longer inherits catalog IDs, even after refresh. Curate desired IDs with the catalog checkboxes or editor to migrate from 0.2.0. The `kept-config` fallback is removed because configured IDs always remain the picker list.
- `LoadedProvider.models` carries an ID and catalog-membership status per entry. Catalog metadata/errors are separate fields; `Registry` retains original config bytes for stale-save detection. Non-terminal output adds membership, empty-reason, and error lines after provider summaries.
- Integrated main's local echo-backed chat screen: Enter on a curated model writes `selection` and opens chat. `/model` reloads the curated picker locally; switching preserves the transcript and cancellation keeps the active model. Quitting the initial picker exits without resuming an old selection. Replies and token counts are stand-ins, not real model inference.
- OpenAI login uses the current ChatGPT direct-token flow, not the old Codex public-client flow. Pasted input must be a full callback URL with state and issued client ID. Conflicting OpenAI proxy origins are refused rather than receiving browser tokens; register proxies as separate gateway IDs.

### Added

- Explicit catalog refresh with `r` or `ahi tui --refresh`. In a pipe, failed refreshes retain/print the local load and return exit status 1.
- `c` opens catalog curation: Up/Down or j/k moves the highlight, Shift+Up/Down or Shift+j/k reorders the highlighted checked model, Space toggles inclusion, and each successful change atomically saves `models.json`. Checked IDs appear first in saved order; configured IDs missing from the catalog remain visible, checked and removable. `y` copies an ID using an available clipboard tool or prints it on exit.
- `e` opens `models.json` in VISUAL/EDITOR (notepad fallback on Windows); editor-only reload preserves exact bytes. Explicit checkbox/reorder saves may create the file and pretty-print JSON while preserving other providers, settings and object-key order. Stale external edits and failed saves preserve the previous loaded registry and file bytes.
- OpenAI entries may curate models. All curated IDs remain visible, marked listed, absent from catalog or catalog unavailable. Empty lists explain how to edit or curate.
- Catalog objects record provider, source URL, RFC 3339 retrieval time and reported IDs. Writes use exclusive `.json.tmp`, sync and rename; failed refreshes retain previous bytes. Old arrays still load and show `retrieval time unknown (old catalog format)`, referring only to a missing cache timestamp, not OAuth.
- Current OpenAI direct-token OAuth includes dynamic issued-client registration, per-installation public UUID, PKCE/state/nonce, listener readiness before browser launch, strict callback parsing/cleanup, issued-client persistence, expiry and bounded refresh rotation. Credential replacement is atomic and mode 0600 on Unix. Catalog refresh uses only the fixed Platform resource with new marked credentials; tokens/response bodies are never printed or stored in catalogs.
- JSON/SARIF report object keys remain sorted despite order-preserving config serialization; existing report goldens are unchanged.

### OpenAI migration and discovery limitation

The previous fixed-client flow issued Codex subscription credentials. Existing credentials remain untouched until a validated new `ahi login` succeeds; old credentials are not silently sent to Platform or Codex. The earlier manual Platform `/models` HTTP 403 used an old-flow credential and is not evidence about new direct-token grants.

A subsequent live `/v1/models` request with a new direct-token credential returned HTTP 200 and `models[].slug` metadata. The decoder now accepts that format and standard `data[].id`, preserving all reported IDs in order, with local HTTP regression coverage. Automated checks have not exercised the interactive browser flow. Discovery remains best-effort; a permission failure preserves cached catalogs and curated selections. Pi uses this grant for Responses, not Decisions, and uses bundled model definitions; ahi copies no default model list. The integrated chat screen uses a local echo stand-in, not a real model inference backend. No Codex fallback or real completion request was added.
