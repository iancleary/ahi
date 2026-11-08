# Changelog

## Unreleased

### Added

- `ahi tools init [--example <name>] [--write] [--force]` writes a shipped tool policy from `examples/tools` into the working directory as `ahi.tools.yaml`, following `ahi migrate`: it prints the policy and writes only with `--write`, replacing an existing file only with `--write --force`. The default is `read-all`. `ahi tools list` names the shipped policies with what each lets the model do. The examples are embedded in the binary, so `cargo install` has them; a test keeps the list and the directory in step.
- `ahi doctor` reports the working directory's `ahi.tools.yaml`: absent, invalid, or present with approve/ask/deny rule counts and the tools it advertises.

### Changed

- The chat startup header's "tools off" line now says that `ahi tools init` writes a policy.
- The repository's own `ahi.tools.yaml` now begins with the `read-all` example rule for rule (a test keeps them equal) and adds `run_command` rules for the repository workflow: `cargo check`, `cargo test`, `cargo doc`, `git status`, `git diff`, and `git log` run without a prompt; `cargo clippy`, `cargo fmt`, other `cargo` subcommands, and `just` ask; `cargo publish`, `cargo install`, and `git push` are denied.
- Example policies: `commands.yaml` gains the root `.env` deny the other reading policies have; `ask-every-read.yaml` asks for `find_files` and `search_files` too; `sources-only.yaml` approves find and search under `src` and `docs`. Comments and the examples README now say that `path: .env` names the root file only, since rules match a path prefix, and that the `sh` rule in `commands.yaml` covers any `sh` command run from `scripts`.

## 0.13.2 (2026-11-08)

### Added

- `scripts/check-published-members.sh` compares each in-repo member crate (`ahi-win32`, `ahi-widgets`) with its published copy on crates.io and fails when the same version has different contents, naming the files and the fix. It runs in CI, `just check`, the release script before tagging, and the publish script before a member is skipped, so the 0.13.0 failure (a Dependabot bump inside `ahi-widgets` with no version bump) cannot reach a release again. `ahi-win32` is 0.1.1: its published 0.1.0 predates a test-only change in the tree.

## 0.13.1 (2026-11-07)

### Fixed

- `ahi-widgets` is 0.1.1, re-published with its `ratatui-core =0.1.2` pin. The 0.13.0 crate never reached crates.io: the registry's `ahi-widgets 0.1.0` still pinned `ratatui-core =0.1.0`, which conflicted with ahi's lockfile after a dependency bump changed the widgets crate without bumping its version. The 0.13.0 GitHub release and its binaries are intact; `cargo install ahi` gets 0.13.1 with the same features.

## 0.13.0 (2026-11-07)

### Added

- Chat transcript formatting ([#52](https://github.com/iancleary/ahi/issues/52)). Assistant text renders a markdown subset (headings, emphasis, inline and fenced code with a `copy` affordance, lists, quotes, rules, links, tables) with in-repo per-line syntax highlighting for eleven languages; `/plain` shows text as written. Tool results open with one header grammar, `tool target · summary · duration`: `edit_file`/`write_file` show a unified diff with red/green backgrounds and word-level emphasis, `run_command` shows the approved argv with `stdout`/`stderr` blocks, and `search_files` rows are clickable `path:line` references. URLs and workspace file references are underlined; a click (or `Enter` in keyboard selection) opens a URL in the browser or a file in the editor chosen by `/editor` (`code`, `cursor`, `zed`, `subl`, `idea`, or an argv template), with `$AHI_EDITOR`, `$VISUAL`, `$EDITOR`, then `code` on `PATH` as defaults. The activity line counts the turn's elapsed time, each tool header carries its call's duration, and a completed turn ends with a right-aligned footer `done in 1m 12s · 14:02` in local time from a TZif reader (`/clock utc` for UTC). Selections copy rendered text by default; `Y` or `/copy raw` copies the model's markdown source. A fresh transcript opens with a text wordmark header showing the version, model, thinking level, and workspace. Preferences persist in `<state>/preferences`. No new dependencies: the markdown parser, lexers, Myers diff, link scanner, and TZif reader are in-repo modules with tests.

## 0.12.0 (2026-11-07)

### Added

- A weekly `dependency audit` workflow runs cargo-deny and cargo-audit against `main` and opens or updates one issue when either fails. Dependabot updates Cargo pins and GitHub Actions weekly in grouped pull requests, security updates alone; merges stay manual.

### Changed

- ahi no longer depends on the `ratatui` facade or `ratatui-widgets`. It uses `ratatui-core` and `ratatui-crossterm` directly, a 40-line terminal module replaces the facade's `init` and `restore`, and the six widgets it draws with (`Block`, `Borders`, `Paragraph`, `List`, `ListItem`, `ListState`, `Clear`) are forked into the in-repo `ahi-widgets` crate from `ratatui-widgets` 0.3.0 under MIT with the upstream copyright, a per-file provenance header, and a `FORK.md`. Rendering is unchanged; every screen test passes untouched. Nine crates leave the lockfile, `time` among them.
- `serde_yaml` (unmaintained, RUSTSEC-2024-0449) and `unsafe-libyaml` are gone. Every ahi config is parsed by `yaml-rust2`, which already ran the hardening pass, and bridged to serde through `serde_json::Value`, so field errors read the same. One behavior change: YAML scalars are no longer coerced into strings, so an unquoted `true`, `null`, or number where a string is expected (an `entry: true` hook, an `argv: [true]` token) is an error that says to quote it; upstream pre-commit applies the same rule. `check-yaml` reports yaml-rust2's parse messages and still rejects duplicate keys. The advisory ignore is removed from `deny.toml`.
- Three direct dependencies replaced by small in-repo modules with tests against published vectors: `base64` (encoding only, RFC 4648 vectors and a 256-byte round trip through a reference decoder), `libc` (the `O_NOFOLLOW` constant per platform, with an open-a-symlink test), and `time` (RFC 3339 format and parse via the proleptic Gregorian algorithms, round-tripped over every day of two 400-year cycles). Catalog timestamps are now written at second precision; stamps with fractional seconds or offsets still parse. `time` itself stays in the lockfile because ratatui's widgets enable it by default.
- Dependencies: ratatui builds with only its `crossterm` feature, ureq moved from 2.12 to 3.5 (one TLS root store, `http` types), `thiserror` was removed as unused, and `base64` moved to 0.23. The lockfile shrank from 285 entries to 193, and every remaining duplicate version (`foldhash`, `hashbrown`, `syn`) lives inside ratatui's tree.
- `deny.toml` now fails on unsound or unmaintained advisories for any crate unless ignored with a reason; RUSTSEC-2026-0253 (`lru`, pulled by ratatui-core, unused by ahi) is ignored with a review date. CI and publish run clippy, test, doc, and build with `--locked`.
- The crate description and README no longer mention the retired `ahi.policy.yaml` or describe the chat tools as read-only.

## 0.11.0 (2026-11-07)

### Added

- `run_command`, a chat tool under a new `action: command` rule kind in `ahi.tools.yaml`. It takes an argv list, a working directory, and a timeout, spawns no shell, and is approved by `match.argv`, a token-by-token prefix, with an optional `match.path` for the working directory and `toolchain_env` as in `ahi.hooks.yaml`. The process gets the hook runner's scrubbed environment plus `AHI_TOOL=run_command`, is killed at its timeout (default 120 s, maximum 600), and returns `exit N` with stdout and stderr capped at 32 KiB each. The approval prompt shows the rendered command line, a session grant is the asking rule's argv prefix, and every attempt appends an output-free line to `audit.log`. Commands run on the host in this alpha. `examples/tools/commands.yaml` shows the grammar.
- Two file-changing chat tools under a new `action: file_write` rule kind in `ahi.tools.yaml`: `edit_file` replaces text that occurs exactly once (or every occurrence with `replace_all`), and `write_file` creates a file (or replaces one with `overwrite`). A rule names `edit_file`, `write_file`, or neither for both, so creating files and editing existing ones are approved separately; a write tool under `tool_call` is an error. Writes are atomic, confined to the working directory, never follow symbolic links, and can never touch `ahi.tools.yaml`, `ahi.hooks.yaml`, `ahi.dev.yaml`, `.pre-commit-config.yaml`, `.gitignore`, or `.git`. The approval prompt shows line counts computed from the arguments, and every attempted write appends a hash-only line to `audit.log`. `examples/tools/edit-sources.yaml` shows the grammar.
- Two builtin read-only chat tools, `find_files` (glob over names or paths) and `search_files` (regular expression or literal, `path:line:text`), both in process with no `rg`, `fd`, or `git` subprocess, honoring `.gitignore`, pruning `.git` and ahi's directories, never following symbolic links, with sorted output and fixed caps. `read_file` takes `offset` and `limit` and, when the 64 KiB cap is hit, ends with the offset to continue from instead of a bare truncation marker. The repository's `ahi.tools.yaml` and `examples/tools/read-all.yaml` approve the new tools.
- `ahi dev` and `ahi dev run` set the container hostname to the image's short name (`ahi-dev-rust` for `ghcr.io/iancleary/ahi-dev-rust:latest`) and pass the mounted directory's name as `AHI_DEV_PROJECT`, so the image's prompt reads `dev@ahi-dev-rust:<version> <project>`.

## 0.10.0 (2026-11-07)

### Changed

- `--help` pages group the four global flags under "Global options" at the end of every page instead of interleaving them with a command's own flags, `ahi run` groups its flags into file selection, output, and behavior, every command has a one-line summary with the caveats moved to `--help`, `--format` and `--color` list their accepted values, and the stale "v0.1" phrasing is gone.
- `ahi migrate` prints a report with aligned columns and counted sections, collects the `ahi.hooks.yaml` rules the local hooks need into one block instead of repeating a snippet per hook, shows the config path relative to the working directory, and prints the "left unchanged" or "wrote" line on stdout at the end.

### Added

- `ahi dev --tag <tag>` and `tag` in `ahi.dev.yaml` choose the image tag (`latest`, `edge`, a dated tag) separately from the image name; the flag outranks the file, which outranks the default `latest`. A reference that already names a tag or digest is used unchanged; `--tag` against it is an error, and a repository `tag` yields to a tagged `--image` or `AHI_DEV_IMAGE`. `ahi dev status` reports where the tag came from.

## 0.9.0 (2026-11-07)

### Changed

- `ahi dev` opens `/bin/zsh` when neither `--shell`, the repository's `shell` in `ahi.dev.yaml`, nor `SHELL` says otherwise, matching the login shell of the ahi-dev images. A repository that wants bash sets `shell: bash`.

## 0.8.0 (2026-11-06)

### Added

- `ahi dev run -- <command>` runs a command in the development image with the same single mount as the shell, pulling first when the image is missing, and exits with the command's code. On Unix `AHI_HOST_UID` and `AHI_HOST_GID` are passed for an entrypoint that remaps its user.
- `ahi sandbox check` reports the containment backend this host offers (Landlock plus seccomp, `bwrap`, `sandbox-exec`, a job object, or none), whether a seccomp filter is already active on the process as inside OpenShell, and the evidence. Detection only; `ahi doctor` prints the same summary in place of `sandbox: not probed`.

## 0.7.0 (2026-11-06)

### Added

- `ahi dev` opens a shell in the development image `ghcr.io/iancleary/ahi-dev:latest` from iancleary/ahi-images through `podman` or `docker`, with the working directory mounted at `/work` and nothing else. `ahi dev build`, `shell`, `status`, and `clean` manage the image; `--image`, `AHI_DEV_IMAGE`, and a `dev-image` file in the config directory override the reference, `--shell`, `--runtime`, `--rebuild`, and `--from <ahi-images checkout>` adjust the run. `ahi doctor` reports the container runtime it would use. Closes #28.
- `ahi.dev.yaml` in the working directory lets a repository prefer an `image`, `shell`, and `runtime` for `ahi dev`. Flags and environment outrank it; it grants nothing.

### Changed

- The repository-level files are named for what they decide, one file per job, with one rule grammar (`id`, `action`, `match`, `effect`, optional `ask`) across the allowlists. `ahi.policy.yaml` is now `ahi.hooks.yaml`: local `system`/`script` hooks are approved by `action: hook` rules matching the hook id and exact entry, a deny rule beats approve, `ask` is refused, and the unused `repo_allow`, `runtime_allow`, and `network_hooks` keys are gone. `ahi.actions.yaml` is now `ahi.tools.yaml` with the same contents, `ahi tui --actions` is `--tools`, and `examples/actions` is `examples/tools`. A retired filename in the repository root is an error with the new name, never silently ignored. `ahi doctor` prints `hooks policy:` and the approve/deny rule counts.
- The `.pre-commit-hooks.yaml` manifest parser is removed from the library. ahi never fetches a hook repository, so nothing read it.

## 0.6.1 (2026-11-06)

### Changed

- The crate description, `--help` summary, and README opening now describe both the hook runner and the terminal chat; crates.io keywords and categories added.
- README: current installation instructions for 0.6.0 and later, one section per subcommand with a linked summary table, and sections ordered for users (Installation, Getting started, Commands, Files and directories, Verifying downloads, Development).

## 0.6.0 (2026-11-06)

### Changed

- Application files follow the XDG Base Directory Specification instead of one state directory: `models.json` in `$XDG_CONFIG_HOME/ahi`, the OpenAI credential and device id in `$XDG_DATA_HOME/ahi`, `selection`, `thinking`, and the hook `audit.log` in `$XDG_STATE_HOME/ahi`, and catalogs with the object store in `$XDG_CACHE_HOME/ahi`. Only absolute XDG values count; Windows without them uses `%LOCALAPPDATA%\ahi\{config,data,state,cache}`. `--state-dir` and `AHI_STATE_DIR` remain as an explicit single-root layout, and `--cache-dir`/`AHI_HOME` still move the cache. The cache no longer falls back to the temporary directory, and `ahi run` writes its audit log without opening the object store. `ahi doctor` prints every location. Nothing is migrated from the old `~/.local/share/ahi` root: move `models.json` to `~/.config/ahi/` by hand, or keep `--state-dir ~/.local/share/ahi`. Library callers see `Directories` in place of the state path. On Unix the `ahi` directories ahi creates are mode `0700` and the files it writes there are `0600`; the credential already was, and `models.json`, the device id, `selection`, `thinking`, and `audit.log` now match. A credential that other users can read is refused with a `chmod 600` hint, and `ahi doctor` warns about any ahi path that is group- or world-accessible.

## 0.5.1 (2026-11-06)

### Fixed

- Cancelling a chat turn during the reconnect backoff now ends the wait immediately: the worker parks for the remaining backoff and `cancel` unparks it, instead of polling the flag every 50 ms.
- The Windows clipboard tests in `ahi-win32` no longer contend for the global clipboard with each other, and the round trip retries once after a pause when another process holds it.

### Changed

- Source layout: the pre-commit hook runner moved to `ahi::hooks` (`config`, `filter`, `git`, `identify`, `check`, `runner`, `output`, `policy`, `store`, `audit`, `migrate`), the picker, gateway, login and command-palette screens to `ahi::tui`, and WSL detection into `clipboard`. Library callers update `ahi::config` to `ahi::hooks::config` and so on; the CLI is unchanged.
- Release and packaging scripts run their embedded Python through `uv run --python 3.14` when `uv` is available, falling back to the system `python3` (3.11+) otherwise. `.python-version` pins 3.14 for `uv run`.

## 0.5.0 (2026-11-05)

### Added

- Drag-selection auto-scroll: holding a left drag on the top transcript row or below the transcript scrolls one row per frame, keeping the original anchor and extending the highlight into newly revealed rows. Returning inside the viewport or releasing stops it; release still copies the exact text across soft wraps.
- Keyboard selection mode: `Ctrl+S` or `/select` places a cursor on the newest visible transcript row. `h`/`j`/`k`/`l` or arrows move by grapheme and row, `w`/`b` move by word with Vim's character classes, `0`/`$`, `g`/`G` and Page Up/Down jump, `v` starts a character-wise visual range and `V` a line-wise one over whole logical lines (trailing newline included, as in Vim), `y` yanks it through the shared clipboard adapters and leaves the mode with the highlight kept for a `Ctrl+Y` retry, and `Esc` leaves without copying. Selection keys never reach the draft or a tool approval, the transcript scrolls to keep the cursor visible, and the activity line shows `SELECT`/`VISUAL` hints.
- The tool approval prompt lists the answers for the call awaiting a decision. `↑`/`↓` move the highlight and `Enter` applies it; the letter keys (`y`, `s`, `n`, `a`, `Esc`) still answer directly. `Shift+↑`/`Shift+↓` move the cursor to another numbered call, so calls can be answered in any order and a decision revised before the batch is sent; the cursor otherwise continues at the next undecided call. "Approve all remaining" and "Deny all remaining" apply to every undecided call and are offered only while more than one is undecided.
- `ahi tui --actions <file>` uses that tool policy file instead of the working directory's `ahi.actions.yaml`. The file must exist and parse (checked at startup and on every turn); rule paths and tool confinement stay relative to the working directory. `examples/actions` holds six ready-made policies, from read-everything to deny-all plus a deliberately invalid one, with a README.

### Fixed

- Native Windows clipboard copies now write `CF_UNICODETEXT` through the Win32 API (no process, no temporary file, no registry dependency), normalizing bare LF to CRLF. If the clipboard stays busy, the PowerShell fallback hands the text over through a private UTF-8 temporary file, as WSL already did. The FFI lives in the in-repo, dependency-free `crates/ahi-win32` workspace crate, so `ahi` keeps `unsafe_code = "forbid"`. Releases publish `ahi-win32` before `ahi`; `just package` verifies both. The clipboard code is split into platform routing, bounded helper execution, OSC 52, and the Windows handoff, and the routing for macOS, native Linux, WSL, Windows, and remote sessions is unit-tested.
- Browser-login OpenAI Responses turns failed with `invalid or conflicting OpenAI response event` as soon as the model finished: the live API now sends `response.completed` with an empty `output` array (and no `Content-Type` header), and the parser required that array to list every streamed item. The finished output items now stand in for an empty terminal `output`, for text and tool-calling turns alike; a terminal event that lists items is still reconciled one for one, and an item still pending still fails.

## 0.4.0 (2026-11-05)

### Added

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
