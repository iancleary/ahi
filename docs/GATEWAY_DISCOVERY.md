# Gateway discovery

## The workflow

Press `/` in **Chat, Models, or Catalog** to open the shared **Commands** popup. **Add gateway** (`/gateway`) is the first action. Type to filter, Up/Down to choose, Enter to open, or Esc to dismiss. You can also type `/gateway` exactly. New installations show a first-gateway prompt; registered gateways with no selected models instead point to catalog curation. See [the product audit and UI design](TERMINAL_EXPERIENCE.md).

The form makes the sequence visible: **1 Connect → 2 Review → 3 Choose models**. It opens a URL text box rather than requiring manual JSON configuration.

1. Enter the gateway origin or **inference base URL**, for example `https://gateway.example` or `http://127.0.0.1:11434/v1`. If the catalog does not declare `baseUrl`, your entered URL is the inference fallback and must include its required path.
2. Press Enter to discover the gateway. This explicitly requests `GET <origin>/agent/models.json`, with the entered URL's path removed. It sends no authentication or cookies and follows no redirects.
3. Review whether this is a new registration, rediscovery, or an identifier conflict. The screen shows the provider identifier, inference base URL, effective authentication marker/environment reference, and model preview. Existing authentication and header settings are retained and described rather than misleadingly displaying catalog defaults. Discovery alone does not write anything.
4. Press Enter again to register the gateway and cache its catalog. Press `e` to edit the URL instead, or Esc/Ctrl+C to cancel.
5. The provider's **Catalog** opens automatically with the registration result and any cache warning visible immediately, including when onboarding started in chat. Press **Space** on each model you want to make available. Press **Enter** on a checked model to use it and start/resume chat directly. Enter on an unchecked model explains that it must be added first. **Esc** returns to Models, retaining the provider and highlighted checked model when possible. From a picker opened inside chat, `q` cancels and returns to the existing chat without switching its active model; `/quit` exits the application instead. Onboarding-specific step labels disappear after you leave that catalog.

The URL field supports typing, bracketed paste, Left/Right, Home/End, Backspace/Delete, and Ctrl+U to clear. Input is capped at 8192 bytes. Chat defers the command while a response is active, like other settings except quit: submit the preserved draft again after the response finishes.

The inference path is **never guessed**. A catalog's validated same-origin `baseUrl` takes precedence for a new registration: entering `https://gateway.example` with a returned `https://gateway.example/v1` configures `/v1/chat/completions`. Without a returned `baseUrl`, the entered URL is used unchanged. Review displays the final URL before saving. A different returned origin is rejected, and refresh never redirects an existing registration. Do not enter the catalog endpoint itself unless that is genuinely your inference base URL.

## Discovery is not automatic model selection

The catalog identifies the gateway; ahi does not derive its identifier from the hostname. A supported response looks like:

```json
{
  "id": "lab",
  "api": "openai-completions",
  "apiKey": "gateway",
  "models": [
    {
      "id": "vendor/model",
      "input": ["text"],
      "cost": {"input": 0, "output": 0}
    }
  ]
}
```

`provider` may replace `id`; if both exist, they must agree. `api` may be `openai-completions`, `openai-responses`, or `anthropic-messages`; registration saves the declared value and Review shows it. `apiKey` must be the non-secret marker `gateway` or a valid `$NAME` environment reference. Inspect that reference before confirming registration: its value will be resolved from the environment only when you later submit a model request. Literal secrets are rejected. Optional prices are finite nonnegative numbers in USD per million tokens. Supported metadata includes names, context/output/request limits, text/image capability, reasoning, costs, and typed compatibility flags. Unsupported fields are rejected rather than silently treated as implemented features. `openai` is reserved for browser login, not URL discovery.

The supported Pi representation is also accepted:

```json
{
  "providers": {
    "lab": {
      "baseUrl": "https://gateway.example/v1",
      "api": "openai-completions",
      "apiKey": "gateway",
      "models": [{"id": "vendor/model", "contextWindow": 262144}]
    }
  }
}
```

Its map key identifies the gateway. A unique compatible provider is selected; if several are present, enter the exact inference base URL to select a unique match. Registered refresh uses the saved provider identifier. See [Gateway model configuration](GATEWAY_MODEL_CONFIGURATION.md) for the complete supported subset, metadata precedence, and request behavior. This does not evaluate Pi credential commands or authorize tools.

Confirmation registers a new entry in `<state>/models.json` like:

```json
{
  "providers": {
    "lab": {
      "baseUrl": "http://127.0.0.1:11434/v1",
      "api": "openai-completions",
      "apiKey": "gateway",
      "models": []
    }
  }
}
```

The discovered IDs and their typed metadata go into `<state>/catalogs/lab.json`, not the curated `models` list. Explicit Space actions in the catalog then add models. This preserves ahi's separation between reference discovery and operator selection. Registration does not write `selection`, alter OpenAI credentials, or send chat messages.

## Ownership and failure behavior

- [`src/gateway.rs`](../src/gateway.rs) owns the URL form, background fetch, preview, and confirmation. It reuses the existing Unicode-aware input editor. The fetch worker returns a candidate and never writes state, so cancelling a pending request cannot register a gateway later. The in-flight request can continue until its network limits are reached; cancellation does not promise immediate network termination.
- [`src/harness/gateway.rs`](../src/harness/gateway.rs) validates the entered URL and registers a validated candidate. [`src/harness.rs`](../src/harness.rs) supplies the same catalog decoder and HTTP client used by existing gateway refreshes.
- [`src/harness/curation.rs`](../src/harness/curation.rs) performs configuration saves with snapshot conflict checks, an exclusive temporary file, validation, and atomic replacement.
- [`src/commands.rs`](../src/commands.rs) owns the context-aware command registry, common actions, shared popup, and explicit picker outcomes. All three screens use it; chat-specific controls are not offered in model lists.
- [`src/tui.rs`](../src/tui.rs) and [`src/chat/mod.rs`](../src/chat/mod.rs) restore terminal input modes and carry typed registration/cache outcomes into the destination catalog. The transcript and active chat selection survive the handoff. Only checked-model Enter explicitly changes that selection.

Only HTTPS is allowed outside loopback; HTTP is permitted for localhost, 127.0.0.1, and ::1. Userinfo, query strings, fragments, whitespace, control characters, and backslashes are rejected. Catalog requests use the existing 15-second connection/read/write timeouts, 1 MiB body cap, and no-redirect policy. A successful catalog fetch does not prove that inference or a configured environment credential will work.

Existing provider settings are protected:

- Same identifier and same base URL (ignoring trailing slashes): update the cached catalog only. Preserve curated IDs, headers, authentication settings, key order, and original configuration bytes.
- Same identifier at a different URL: reject registration rather than silently redirecting future chat requests.
- External configuration edit while the view is open: reject confirmation. Cancel and reopen `/gateway` to load the new configuration before retrying.
- Configuration save failure: leave the existing configuration unchanged. Review any `models.json.tmp` conflict rather than deleting another writer's file.
- Catalog save failure **after** registration: the gateway remains registered with no automatic model selection. The notice explicitly says the catalog save failed; use `r` to refresh after resolving the cache problem. Configuration and catalog files are individually atomic, not a two-file transaction. Rediscovery at the same URL can also retry the cache write without rewriting configuration.

Gateways without this gateway-specific discovery endpoint still require manual `models.json` configuration. This command does not implement arbitrary OpenAI `/models` discovery, authentication setup, or automatic inference-path probing.

## Verification

Use an isolated build directory to ensure the PTY runner exercises the intended checkout's executable rather than a previously packaged artifact:

```sh
cargo fmt --all -- --check
cargo clippy --target-dir target/terminal-experience-verification --all-targets --all-features -- -D warnings
cargo test --target-dir target/terminal-experience-verification --all-features
cargo build --target-dir target/terminal-experience-verification
uv run scripts/smoke-model-picker.py \
  --binary target/terminal-experience-verification/debug/ahi \
  --capture-directory target/terminal-experience-captures
```

The maintained POSIX runner uses disposable state and a real loopback HTTP server. It exercises empty-picker URL entry and editing, invalid URLs, preview cancellation, registration, explicit curation, model selection, and streamed chat through the registered gateway. It also proves Pi-shaped registration, authoritative `/v1` inference, metadata persistence/restart, compatibility-driven requests, exact byte-limit rejection without a request, and provider ambiguity/invalid-metadata retention. It covers shared Commands filtering/navigation/dismissal in all three screens, command paste, direct checked-model Enter, provider/model focus retention, nested application exit, discovery from catalog curation, strict response validation, refusal to follow redirects, different-URL conflicts, stale-save retention, rediscovery, pending-fetch cancellation, and partial catalog-save failure. Existing OpenAI and picker workflows remain in the same runner. Optional `.txt` reconstructed screens and `.ansi` streams show onboarding at 80×24 and 60×24; they are evidence for the scripted disposable workflow, not a full terminal emulator or live-provider proof. No external service or paid model request is needed. The runner's OpenAI cases require local port 1455 available.

Live remote discovery remains dependent on the remote server's published catalog and TLS configuration. Verify it deliberately with the intended URL; do not treat the local fixture as evidence that an untested remote gateway is reachable.
