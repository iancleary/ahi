---
name: verify-gateway-discovery
description: Verify ahi's shared Commands popup, Pi-shaped catalog normalization, typed model metadata, and gateway onboarding through local streamed chat. Use after discovery schema, model metadata, request compatibility, registration, or terminal navigation changes.
---

# Verify gateway discovery

## Scope and prerequisites

Read `docs/TERMINAL_EXPERIENCE.md` for the shared interaction contract, `docs/GATEWAY_DISCOVERY.md` for persistence boundaries, and `docs/GATEWAY_MODEL_CONFIGURATION.md` for supported fields and effective metadata, from the ahi repository root. Use the repository's Rust toolchain and `uv`; the maintained terminal runner requires POSIX and Git. Its OpenAI regression cases also require local port 1455 available. Do not install dependencies, stop unrelated listeners, or contact a remote gateway without the required authorization.

Inspect `git status --short` and preserve existing work. Do not launch the user's saved chat configuration to verify this workflow: model requests may cost money or send private conversation data. The runner uses disposable state and its own real loopback gateway.

## Verified sequence

```sh
cargo fmt --all -- --check
cargo clippy --target-dir target/terminal-experience-verification --all-targets --all-features -- -D warnings
cargo test --target-dir target/terminal-experience-verification --all-features
cargo build --target-dir target/terminal-experience-verification
uv run scripts/smoke-model-picker.py \
  --binary target/terminal-experience-verification/debug/ahi \
  --capture-directory target/terminal-experience-captures
```

Use the explicit executable path so a stale default build or packaged artifact cannot substitute for the checkout under review. Reuse the maintained runner rather than reconstructing PTY handling or a separate gateway simulator.

Confirm these observable outcomes:

- `/` opens the same Commands popup in empty Models, Catalog, and Chat. Add gateway is first; filtering, keyboard selection, dismissal, aliases, and single-line picker command paste behave consistently. Paste never submits.
- Empty guidance distinguishes a new installation, an already registered but uncurated gateway, and an unavailable catalog. Inspect the 80×24 captures, not only the pass message.
- URL editing and validation precede an unauthenticated canonical catalog request.
- The supplied two-model Pi-shaped `providers` document and legacy flat schema both work. Entering an origin with a returned same-origin `/v1` shows, saves, and uses that inference path; cross-origin returns fail. Ambiguous candidates require an exact URL, while registered refresh selects by identifier.
- Typed metadata survives cache/restart and curation; explicit local provider/model fields overlay cached metadata. Inspect model-name/limit details and the known context meter, not only the pass message.
- Discovery and preview cancellation make no state writes.
- Confirmation registers an empty curated list and stores a separate reference catalog.
- Connect → Review → Choose models makes each persistence boundary visible. Space explicitly curates; Enter on a checked catalog model explicitly selects and leads to an actual streamed local response at the reviewed inference path. Unchecked Enter only explains the required action.
- Catalog Back retains a non-first highlighted checked model without writing selection or curation. Onboarding labels end on leaving the view. `/quit` exits from nested pickers; `q` cancels back to chat where applicable. Chat retains transcript, conversation context, and command drafts during active responses.
- Invalid metadata, unsupported protocols, redirects, conflicting gateway URLs, and stale configuration do not replace existing state. Paid decimal price metadata is accepted; do not restore the old integer-zero restriction.
- Actual HTTP requests reflect provider/model overrides, conditional usage/store/reasoning fields, both output-token parameter names, and capped output budgets. Finish inference only occurs at `[DONE]` when explicitly declared; truncation and malformed tool calls still fail.
- Exact serialized UTF-8 byte boundaries are exercised, and rejected bodies produce no server request. Reported pricing does not double-count cached input; unknown cached counts/prices remain unknown. No paid external inference is involved.
- Same-URL rediscovery preserves configuration bytes and curated IDs.
- Cancelling a pending fetch cannot cause delayed registration.
- Catalog persistence failure after configuration registration is reported immediately inside the destination Catalog, including entry from chat; refresh recovery becomes visible there. Do not treat it as an unchanged-state failure.
- Review/confirmation controls remain visible in the 80×24 and selected 60×24 captures. Use the maintained optional capture argument, not a second snapshot tool. Captures must contain only disposable fixture data and remain outside the skill package.

## Diagnosis and recovery

- **Old behavior despite a successful build:** compare the executable path with the build target directory. Rebuild in the isolated directory before diagnosing application routing.
- **PTY failure:** inspect the runner's reconstructed screen. Terminal cell differences are not plain output lines. Keep server threads stopped until after the runner forks its PTY child; forking a multithreaded process can hang on macOS.
- **Invalid response event from the fixture:** inspect the real SSE schema, including choice indices. Do not weaken production parsing to accept an invalid fixture.
- **Stale registration view:** cancel and reopen `/gateway`. Do not overwrite another editor's changes.
- **Catalog temporary-file conflict:** inspect ownership and active writers before removing anything. Registration may already exist; retry explicit refresh or same-URL discovery after resolving the cache problem.
- **Remote or Windows behavior:** local POSIX evidence does not establish remote reachability, authentication, inference support, or native Windows terminal behavior. Report those boundaries accurately.

No release, publication, installation, hook activation, or production configuration write belongs to this procedure. The existing runner owns its disposable data and shuts down its local server.
