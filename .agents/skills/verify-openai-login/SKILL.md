---
name: verify-openai-login
description: Verify ahi's OpenAI browser-login lifecycle, Responses chat transport, and safe function-call boundary with maintained local checks. Use after OAuth, request routing, stream parsing, or login handoff changes; live sign-in and inference require separate authorization.
---

# Verify OpenAI login

## Prerequisites and scope

Work from the ahi repository root with its pinned Rust toolchain and `uv` available. The terminal smoke runner requires POSIX, Git, and free local TCP port 1455. Do not install missing tools or stop a process occupying the port without authorization.

Read `docs/OPENAI_LOGIN.md` and `docs/OPENAI_INFERENCE.md` for ownership, request/stream contracts, and operational limits; consult `docs/TOOL_CALLS.md` for the execution-disabled boundary. Keep tokens and callback URLs out of logs and repository files. Use the existing `scripts/smoke-model-picker.py` runner; do not duplicate its PTY implementation or introduce configurable production OAuth endpoints merely for verification.

## Sequence

1. Inspect `git status --short` and preserve existing work.
2. Run formatting, linting, and the existing Rust tests:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --offline --target-dir target/openai-inference-verification --all-targets --all-features -- -D warnings
   cargo test --offline --target-dir target/openai-inference-verification --all-features
   ```

3. Build the actual executable and run the maintained terminal workflow:

   ```sh
   cargo build --offline --target-dir target/openai-inference-verification
   uv run --offline scripts/smoke-model-picker.py \
     --binary target/openai-inference-verification/debug/ahi \
     --capture-directory target/openai-inference-captures
   ```

4. Confirm that the runner reports standalone headless login diagnostics and listener cleanup; login entry from models, catalog, and chat; shared Commands completion; port-conflict recovery; invalid callback validation; cancellation/reentry; browser denial; and preservation of existing state and transcript. The runner captures browser launch in disposable executables and sends only loopback callbacks; it does not contact OpenAI or use real credentials.
5. Review the diff for terminal restoration on every exit and for callback-worker cleanup. The TUI must not invoke the standalone command's blocking stdin reader.
6. Confirm the existing HTTP fixture cases also exercise Responses serialization and real streamed Unicode/history/usage; expiry refresh then stream consumption; one pre-stream 401 recovery; credential retention on refresh failure; malformed/truncated/refused responses; complete interleaved function calls; and safe execution-disabled behavior. Also verify identical valid SSE works with incorrect/missing Content-Type, while non-SSE JSON/HTML, malformed events, and truncated streams fail with HTTP/media-type context and without synthetic private values. Do not restore a header-only acceptance gate or add an unverified JSON-success fallback. Do not confuse internal fixture endpoint injection under `cfg(test)` with a supported production endpoint override.
7. Review production routing: the reserved provider must be curated, pin the public Responses endpoint, reject conflicting URL/key/headers, and never derive Debug for credentials or connection headers. No ambiguous/post-stream generation retry, argument logging, automatic tool execution, or fabricated result is acceptable.
8. Report local verification separately from live OpenAI authentication/inference. The PTY does not complete a successful OpenAI request in the shipped binary. Real HTTP fixtures prove the shared private transport/auth implementation, not acceptance of a live grant, subscription, model, effort, or quota.

## Failures and recovery boundaries

- **Port unavailable:** determine whether another login is pending. Cancel that session deliberately or wait for the port to become available, then rerun. Never terminate an unrelated process automatically.
- **Stale executable:** use the isolated build directory and explicit binary above; do not infer source correctness from an installed executable or default target artifact.
- **PTY timeout:** inspect the reported reconstructed terminal screen and command sequence before changing application behavior. The runner interprets terminal cell differences, not plain output lines.
- **Gateway retry timing:** run the three existing real-HTTP cases listed in [Gateway retry validation](../../../docs/release.md#gateway-retry-validation), then require native Windows CI for cross-platform evidence. Recovery must start its listener after a reconnect event, not a fixed sleep; total elapsed time includes operating-system TCP failure detection as well as backoff. Inspect failed runner output rather than repeatedly rerunning a race. Keep the reconnect-count, terminal failure, streamed recovery, and cancellation assertions intact.
- **Unsupported environment:** report POSIX coverage as unavailable; Rust checks alone do not establish terminal behavior on Windows.
- **Live acceptance unverified:** leave that limitation explicit. A separately authorized live check may finish `/login` in a private state directory and observe the success notice and screen return. Catalog refresh is a separate explicit action. Do not print credentials, reuse production state for destructive experiments, or remove any directory not created for the check.

The automated runner owns and removes its disposable data. No installation, hook activation, release, or publication is part of this procedure.
