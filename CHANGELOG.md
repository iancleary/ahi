# Changelog

## Unreleased

## 0.16.1 (2026-11-13)

### Fixed

- `ahi serve`: an `ask` emitted after its last subscriber had gone, or one no client ever answered, left the turn open and the session `conflict` until the server restarted. The ask is now sent first and tested after: one nobody heard is answered `Unattended` at once; while one waits, the subscribers are probed every second with a `keepalive`, and the asks are settled `Unattended` when the last subscriber leaves or after ten minutes.
- `ahi serve`: a panic on a turn thread left the turn open for good. The turn now closes `failed` with `error.kind: turn` ("the turn thread panicked"), waiting asks are answered `Unattended`, the `turn` event is emitted, and the next `turn.submit` is accepted.
- Sessions: a torn last line (a write cut short) was skipped on read but appended after on write, merging two lines and making the file unreadable for every reader. A session is now written only while held: `hold` takes an advisory lock on the file (`flock`, through `ahi-unix` 0.2.0), cuts the torn line, and refuses with `conflict` when another process holds it, so the server, a standalone chat, and `ahi session import` never write one file together. `ahi serve` lets go of a session with no open turn and no subscriber every 15 s, releasing its file.
- `ahi serve` bounds every `turn.submit` field once, for every caller: `run_id` is 1 to 64 letters, digits, `-`, or `_` (the same rule as `--run-id`), `timeout` is 1 to 3600 whole seconds (a huge value used to panic the connection thread when added to the clock), `prompt_sha256` is 64 hex digits, and `client` is at most 64 printable bytes; anything else is refused as `usage` before a thread or a record is named.
- The client side of the protocol (`ahi exec`, `ahi tui`, `--via`, `delegate`) caps a line from its peer at the same 2 MiB the server does and drops the connection past it, so a misbehaving worker cannot grow a coordinator's memory with one endless line. A peer's refusal message and a worker's `error.message` are kept to 4 KiB. A worker's session and run ids are checked like a client's before they reach the ledger and the audit log.
- `--via` and `delegate`: the connect command now gets `SSH_AUTH_SOCK` and `SSH_AGENT_PID` with the scrubbed environment, so `ssh host ahi connect` authenticates with the agent instead of failing or wanting a plaintext key. Its stderr is captured (last 8 KiB) instead of being written over a raw-mode screen: `ahi exec` repeats it after a failed call, and `delegate` writes it to the coordinator's stderr when that is not a terminal, never into model text. A cancelled delegation waits a few seconds for the worker's word and then returns `cancelled` with `effects: unknown` instead of waiting out the worker's whole timeout.
- `ahi exec --via`, `ahi tui --via`, and `AHI_DELEGATE_VIA` were each split on whitespace, so a path with a space broke them. One parser now splits every connect command like a shell would without being one (single and double quotes, backslash, no expansion, at most 64 tokens and 8 KiB), `{worker}` is filled in after the split so a label is one token, the resolved argv is written into the ledger and the `audit.log` line of each delegation, and `ahi doctor` shows the active template parsed.
- `ahi exec` waits for the `turn` event no longer than the turn's timeout plus the service's two graces, and exits 6 naming the run when the service never reports it.

## 0.16.0 (2026-11-13)

### Added

- A new in-repo workspace crate, `ahi-unix`, holds the two POSIX calls ahi needs and `std` does not expose: a signal handler that records SIGINT, SIGTERM, or SIGHUP for a loop to poll (first arrival wins, default disposition restored so a second copy ends the process), and `kill` aimed at a process group. It has no dependencies, compiles to stubs elsewhere, and like `ahi-win32` is published before `ahi`; `ahi` itself keeps forbidding unsafe code.
- Every tool result event carries a record of what the call did (path and hashes for a write; argv, working directory, exit code, and whether a process ran for a command), and the audit line is derived from that record, so a ledger built from the events and `audit.log` cannot disagree. Audit lines name the front end that ran the turn (`origin`, `tui` today) and the turn's `run_id` when it has one.
- The turn engine takes a deadline: no tool call starts past it and a command's own timeout is clamped to the time that remains. An approval may be answered `Unattended`, a denial whose reason tells the model that no operator is attending. Both are used by the headless and served turns that follow.
- Every chat is recorded as a session: one file of JSON lines under the state directory (`sessions/<id>.jsonl`, owner-only) holding the operator's messages, the model's replies and tool calls, each tool result, `/model` and `/thinking` changes, and one `turn` line per exchange with its outcome (`ok`, `failed`, `cancelled`). Each line is written when it happens, so a crash leaves a visible unmatched call. A failed or cancelled turn stays on disk as a branch and the next turn hangs off the last success. `ahi tui --no-history` turns recording off; `/clear` starts a new session at the next turn.
- `ahi tui --resume <id>` continues a recorded session with its model, thinking level, and the messages on its active path. `ahi session list [--json]`, `ahi session show <id|path> [--json] [--branch <seq>]`, `ahi session export <id> --to <file>`, and `ahi session import <file>` read the store and move session files in and out of it, so a session written on another machine can be listed and resumed here.
- `ahi exec`: one model turn from the command line. The prompt comes from `-p`, `--prompt-file`, or stdin; tools are confined to `--workdir` under its `ahi.tools.yaml` or an explicit `--tools` file outside it; `--headless` takes the prompt only from a file, denies every `ask` with a reason the model sees, and writes a result document to `--result-file` (status, text, error, usage, tool counts, `effects`, and a ledger matching `audit.log`). stdout carries the answer or, with `--format json`, the document; stderr the progress lines; the exit code how the turn ended (0 ok, 1 failed, 2 usage, 3 configuration, 4 credential, 5 policy, 6 cancelled or timed out, 7 session, 8 result file). `--timeout` bounds the whole turn, signals cancel it, `--run-id` makes it idempotent per session, `--prompt-sha256` refuses a changed prompt, `--session` continues a recorded session, `--check` validates without a turn.
- `ahi serve`: the session service on a Unix socket, one process per user per host, owning the session store and running every turn through the same engine as the chat. One JSON object per line in each direction, the shape herdr-core uses for its own cores: requests for sessions, turns, settings, approvals, and cancels; events for entries, streamed text, asks, and turn results on a subscribed connection. No network listener, no token, no TLS; the socket is mode 600 and ssh carries the bytes across machines. `ahi serve stop` ends it. `ahi connect` bridges stdin and stdout to the socket, so `ssh host ahi connect` is a remote connection. `ahi exec` uses the server when one answers and runs the same service in its own process otherwise; it never starts one. Design in `docs/SERVE.md`.
- `ahi tui` runs its turns through `ahi serve`: when no server answers on the socket the chat starts one detached (output in `<state>/serve.log`) and attaches; the server records the session and outlives the chat. The screen is unchanged: the same transcript, tools, approval dialog (answered over the socket), `/model`, `/thinking`, `/clear` (a new session), and `--resume`. `--standalone` keeps the engine inside the chat; `--socket` names another server. `ahi serve start` starts a detached server when none answers and prints the socket path.
- `delegate`, a builtin tool governed by `action: delegate` rules that approve worker labels (`match: {worker: rust-ci}`): the model hands a task to a worker, another `ahi serve` reached through the connect command `AHI_DELEGATE_VIA` names (default `hc connect --worker {worker}`), waits for its turn, and receives the answer, the status, and the worker's `effects`; a `task` id continues the same worker conversation. The ledger and `audit.log` record the label, the worker's session and run ids, status, and effects, never the task text. `examples/tools/delegate.yaml` shows the rules. A repository names labels only; where a label lives is herdr-core's.
- `ahi tui --via <command>` and `ahi exec --via <command>` attach to a server elsewhere through a command whose stdin and stdout carry the protocol (`ssh host ahi connect`). On the server, `session.create` without a directory opens the session where the server runs, and without a model uses the host's saved selection, so a worker server started inside a checkout needs nothing else.

### Fixed

- Cancelling a turn (`Esc`) in the middle of a tool batch now stops the batch: the running command is killed together with every process it started, since the program leads its own process group on Unix, and the calls after it are denied as `turn cancelled` instead of running. Before, a kill reached only the program itself, so a helper it had spawned kept the output pipes open and the call waited for it, and the rest of the batch, including writes, still ran after the cancel.

## 0.15.0 (2026-11-12)

### Added

- `read_file` results in the chat show each file line behind its line number, starting at the requested offset. The tool's own `[lines A-B of N]` and "more lines" notes stay dim and unnumbered.
- The chat reports what it is doing to the terminal multiplexer that holds it with OSC 7501, the Program Status Protocol herdr-core reads: `idle` when open, `working` while a turn runs (naming the tools being called), `blocked` with `kind=permission` while a tool call waits for approval, `done` with the first line of the answer, `error` with the failure's kind, and `clear` on exit. The title is the working directory and model. The sequence is written only when stdout is a terminal; terminals that do not know it ignore it, as with any unknown OSC. Nothing is read back and no dependency is added.

### Changed

- Wrapped rows in the chat line up under their content instead of starting flush left: file lines under the code, diff lines under the code after the old/new gutter, list items under their text after the marker, and quotes after the bar. Selection and copy skip the padding, so a wrapped line still copies as one line.
- The repository `ahi.tools.yaml` lets the model edit existing files under `src` and write `TODO.md`, each after a prompt.

### Fixed

- The chat no longer panics when drawing an `edit_file` or `write_file` diff whose changed line is longer than 400 characters or contains a tab. Word emphasis was computed on the line as written and applied to the clipped, tab-expanded row, which could split the multi-byte ellipsis. Such rows now skip emphasis.

## 0.14.1 (2026-11-08)

### Added

- `ahi tools list` now shows the working directory's `ahi.tools.yaml` painted by outcome, followed by the summary `ahi doctor` prints, or says the file is absent. `ahi tools examples list` (the default for bare `ahi tools examples`) names the shipped policies with what each lets the model do, which `list` printed in 0.14.0; `ahi tools examples show` prints every one painted and `ahi tools examples show <name>` prints one.
- `ahi tools init`, `list`, and `examples` paint each rule in a terminal with the chat's colors for its outcome: approve green, ask gray, deny red, the same backgrounds the transcript uses for added and removed diff lines, with a legend. `--color never` prints plain text, `auto` honours `NO_COLOR`, and the written file is always plain. This is the first command to use `--color`, which was validated and ignored before.

### Changed

- `ahi tools --help`, `ahi tools init --help`, and the `ahi tools examples` list state the boundary: every builtin tool works inside the working directory, rule paths are relative to it, absolute paths, `..`, and symbolic links are refused, and `run_command` starts only inside it; and that this is not a sandbox, since an approved program runs on the host with the operator's access and can follow links, read above the working directory, and run other programs. Example summaries say "the whole working directory" instead of "anywhere".

## 0.14.0 (2026-11-08)

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
