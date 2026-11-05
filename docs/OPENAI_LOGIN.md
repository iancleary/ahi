# OpenAI login

## One authorization session, three terminal entry points

Type `/login` and press Enter in any of these locations:

- The initial curated model picker, including an empty installation.
- The provider catalog curation screen.
- Chat input, where `/login` is also offered by the shared Commands popup.

Press `/` in any of the three screens for the same **Commands** menu, then choose **OpenAI sign-in**. Exact `/login` entry remains available.

All three open the same login view. Login always means **Sign in with ChatGPT for OpenAI**, not authentication for the highlighted gateway. Registered gateways continue to resolve their configured environment references separately.

The existing standalone interface remains available:

```sh
ahi --state-dir /path/to/private/state login
ahi --state-dir /path/to/private/state tui
```

In the login view, the browser is attempted automatically and the authorization URL remains visible. If the browser cannot reach the local callback listener, paste the **complete final callback URL** into the view and press Enter. It must contain `code`, matching `state`, and the issued `client_id`. Bare codes, foreign origins, and missing or duplicate parameters are rejected. Invalid pasted input reports a diagnostic and leaves the browser session pending for another attempt.

Callback input is hidden: only its character count is drawn. Backspace deletes a character, Ctrl+U clears the input, and Up/Down scrolls the authorization URL on a small terminal. Esc or Ctrl+C cancels the pending browser wait. Cancellation, failure, and success return to the originating screen. Chat retains its transcript and active model; the picker retains its current view and highlight. During an active model response, chat retains the `/login` draft; submit again after the response finishes.

## Ownership and lifecycle

The separation is between authorization and terminal input, not between two implementations of OAuth:

1. [`src/harness/openai.rs`](../src/harness/openai.rs) owns `OpenAiLogin`: bind numeric-loopback port 1455, create PKCE/state/nonce and the installation UUID, construct the authorization URL, and run a bounded nonblocking callback receiver on a worker thread.
2. [`src/login.rs`](../src/login.rs) owns terminal drawing and input. It polls the session and supplies pasted callback text through the same validator used by the standalone command. It never starts a blocking stdin reader.
3. Once a callback is accepted, the session stops and joins its callback worker before exchanging the code and atomically writing a validated credential. Dropping a cancelled session also stops and joins the worker, releasing its listener and spare connections before reentry.
4. [`src/chat/mod.rs`](../src/chat/mod.rs) and [`src/tui.rs`](../src/tui.rs) hand off the terminal and restore their previous screen afterward. Chat receives only a success/error notice, never the authorization URL, callback text, or tokens. The picker reloads local registry authentication state after success.

The standalone `ahi login` uses the same session but supplies pasted input through a stdin reader. That reader belongs only to the standalone command; invoking it inside a long-lived TUI would leave a blocked reader competing with chat input after an automatic browser callback.

## What login changes—and what it does not

The grant requests `chatgpt.tokens.use.direct` for `https://api.openai.com/v1` using dynamic client registration. The issued client ID is used for code exchange and later renewal. A successful exchange replaces `<state>/credentials/openai.json` atomically, with mode `0600` on Unix. Failure or cancellation leaves any previous credential untouched. ID tokens and account/profile data are not persisted. A stable public installation UUID is kept separately in `<state>/openai-device-id`.

Login does **not** modify `models.json`, fetch a catalog, or write `selection`. Use `r` in the model screen for an explicit catalog refresh afterward. The credential can now be used by the [OpenAI Responses backend](OPENAI_INFERENCE.md) when you explicitly select a curated OpenAI model and submit a message. Login itself sends no inference request. Configured gateways use a separate streaming Chat Completions backend. Tool requests are parsed but execution remains disabled; see [Tool calls](TOOL_CALLS.md).

Operational limits:

- Port 1455 must be available. A conflict fails before launching a browser; cancel the other pending login before retrying.
- Waiting for browser authorization has no deadline. Cancel explicitly when abandoning the login.
- Token exchange is synchronous, with a 15-second request timeout and 1 MiB response cap; terminal cancellation is processed during the browser wait, not during that exchange. Requests do not follow redirects.
- Pasted callback input is limited to 8192 bytes in the terminal view.
- Credential rotation across separate processes is not serialized. Do not log in or refresh concurrently against the same state directory.
- Picker `auth signed-in` still means a credential file exists, not that a live request validated it. Historical catalog refresh errors remain until another explicit refresh.

## Verification and troubleshooting

Run the existing checks and the real-terminal workflow:

```sh
cargo fmt --all -- --check
cargo clippy --offline --target-dir target/openai-inference-verification --all-targets --all-features -- -D warnings
cargo test --offline --target-dir target/openai-inference-verification --all-features
cargo build --offline --target-dir target/openai-inference-verification
uv run --offline scripts/smoke-model-picker.py \
  --binary target/openai-inference-verification/debug/ahi \
  --capture-directory target/openai-inference-captures
```

The POSIX smoke runner uses disposable state, browser-launch capture executables, and real PTYs and loopback callbacks. It exercises `/login` from models, catalog, and chat; slash-menu completion; authorization URL parameters; port-conflict recovery; invalid paste; cancellation and reentry; browser denial; and preservation of credentials, settings, selection, and transcript. It also invokes standalone `ahi login` without a browser, verifies bare-code rejection and credential retention, and checks that no callback listener remains. No external OpenAI request or actual browser is used. It needs local port 1455 free before starting; it deliberately occupies that port briefly for the conflict case.

Existing OAuth and backend tests exercise code exchange, credential validation, file permissions, token rotation, expiry, bounded authentication retries, response limits, and Responses streaming/history/tool-call refusal against local HTTP fixtures. They do not establish that OpenAI currently accepts a new live grant. A live successful browser sign-in and token exchange through `/login` still require operator verification; do not treat the offline checks as that evidence.

For a deliberate live check, launch `ahi tui`, enter `/login`, finish browser authorization, and confirm the completion notice and return to the same screen. Avoid printing or attaching the credential or callback URL. If desired, use `r` separately to verify catalog access; it is not part of login and may have a different authorization outcome. Keep any disposable state outside the checkout and remove only the directory created for that check.
