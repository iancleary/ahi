# Curated picker and main's chat integration

## Outcome

Resumed the paused merge of main at `c62b99ba8a3533097cbf0165f0adaa74e73a0493` into `fix/openai-model-loading` as authorized by the user. The user explicitly chose **main's Enter-to-chat behavior while retaining the curated picker**.

- Retained package version **0.3.0** in Cargo.toml and Cargo.lock.
- Resolved the TUI conflict by keeping the curated picker, catalog checkboxes, persistent ordering, editor reload, explicit refresh, and refresh diagnostics.
- Enter on a curated model writes `<state>/selection` and opens main's local echo-backed chat screen.
- `/model` reloads local settings and reopens the same curated picker. Selecting another model preserves the chat transcript. Invalid external edits retain the last valid registry and display an error.
- Cancelling `/model` keeps the current chat selection. Quitting the initial picker exits, even if an old selection file exists; it does not silently resume a stale model.
- Non-terminal invocation remains headless. Registry loading and model switching do not fetch catalogs.
- The chat backend is an **echo stand-in**, not real OpenAI or gateway inference. No live credentials, new login, token rotation, installation, or real inference were needed for this merge.
- Updated UI help, README, changelog, runtime/spec/chat docs, and the rerunnable terminal smoke check.

## Verification

- Full suite: **126 tests passed** (101 library + 12 CLI + 13 offline).
- Real PTY smoke passed: all previous curation/editor/save-conflict checks, Enter-to-chat, stand-in reply, locally reloaded `/model`, switching/cancellation, transcript retention, embedded-slash IDs, and Ctrl+D chat exit.
- Build, formatting, Clippy with warnings denied, rustdoc with warnings denied, and diff whitespace checks passed.
- The first full parallel run hit the previously observed callback port-rebind flake in `callback_rejects_state_mismatch_with_http_error_and_cancel_releases_listener`. The unchanged full-suite rerun passed. The unrelated callback implementation/tests were not changed or claimed fixed.

Reproduce checks:

```sh
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=init.defaultBranch GIT_CONFIG_VALUE_0=master cargo test --all-features
cargo build
python3 scripts/smoke-model-picker.py
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS=-Dwarnings cargo doc --all-features --no-deps
git diff --check
```

## Delivery

The earlier help-text commit `6b20db0` was already pushed before the pause. This integration is intended as the merge commit on `fix/openai-model-loading`, pushed to its existing upstream. Inspect `git log -1 --format=fuller`, `git rev-list --parents -n 1 HEAD`, and `git status -sb` for the final delivery state. Do not push main, force-push, or create a release.
