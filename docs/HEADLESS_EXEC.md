# One-shot turns and headless workers (`ahi exec`)

Status: design plan, not implemented. This document is for review; nothing here changes code yet, and the examples below are proposed syntax, not commands available today.

## Roles

Three roles, three programs, and the roles are expressed by which process runs which thing, not by subcommands named after them:

| Role | Who | Owns |
| --- | --- | --- |
| Coordinator (decides) | the model in a main `ahi tui` session, or a human at a shell | what to delegate, to which worker, and what to do with the result |
| Worker (acts) | `ahi exec` on some host | one bounded turn, a working directory, a tool policy |
| Coordination plane (sees and carries) | herdr-core (`hc`) | the worker registry, transport, file movement, task state, history custody, and the human's view of every running task |

The shape follows [beep-boop](https://github.com/jessfraz/beep-boop): one coordinator, N workers, durable per-task conversations, results returned to the main chat. The difference is that the mechanics (hosts, sessions, transport, liveness, steering) live in hc, which is a terminal multiplexer and GUI that opens shells on worker hosts over `ssh` and `tailscale ssh`. ahi performs one turn per invocation and carries continuity in files the caller owns. ahi does not become a daemon, a session store, or a registry of hosts; it never reads hc's configuration, and hc never reads a repository's `ahi.tools.yaml`. This plan assumes only that hc can open a shell on a host, place files, run a command, and read files and the exit status back. Confirm those four with hc before freezing the interface.

```text
hc (coordination plane)                         worker host (local or remote, via a shell hc owns)
  task.md  ───── place ────────────────────▶    ahi exec --headless --prompt-file task.md
  task.tools.yaml (per task)  ── place ───▶          --tools task.tools.yaml
  task.history.json (follow-ups) ─ place ─▶          --history-file task.history.json
                                                     --workdir /path/to/checkout
                                                     --result-file result.json --run-id <id>
  result.json  ◀──── read back ───────────     exit status 0..8 (255 is ssh's own)
  task.history.json ◀── read back ────────
```

Files in, files out, one exit code. Local and remote workers look the same to ahi: only paths cross its boundary, and moving files is hc's job. Each worker host holds its own `models.json` and credential; hc never ships tokens, and the tailnet or the operator's keys authenticate the shell.

## Configuration stays where its authority is

- `ahi.tools.yaml` answers "what may the model do here". It is scoped to a working directory, or to one `ahi exec` invocation through `--tools`. It never names hosts, workers, or credentials: a repository must not be able to say anything about machines.
- Workers, hosts, transports, and placement are hc's registry, local to the host that runs hc. No other machine reads it; a worker learns about its task from the files in front of it and nothing else. This is the property that keeps local and remote workers identical to ahi, and it is why the registry needs no distribution.
- `models.json` stays curation. No `workers:` section.
- ahi gains no new configuration file in this plan. If delegation from a main session lands (see [Later](#later-not-in-this-plan)), a repository bounds it with an `action: delegate` rule naming permitted worker *labels*; hc resolves labels to hosts.

## Two callers, one engine

`ahi exec` is the command a person types, and headless is a strict mode of it chosen in argv, never inferred from the environment.

- A human at a shell wants `-p "text"`, stdin, the working directory's `ahi.tools.yaml`, and `ask` rules that prompt on the terminal. Example: `git diff | ahi exec --model x/y -p "summarize"`.
- hc wants the file-only contract: no stdin, no implicit policy, `ask` answered as deny, a result file, and exit codes.

Inferring the mode from "is stdin a TTY" is wrong here: an hc shell session usually has a pty because the human is watching it, so a TTY-inferred prompt would block a worker forever on a question the model cannot answer. `--headless` is the switch. Everything the model can reach is identical in both modes; only the input discipline and the approval answer differ.

Not `ahi run`: that is the hook runner. Not `ahi worker` or `ahi coordinator`: roles are not subcommands.

## Proposed CLI contract

### Input

- [ ] `ahi exec --model <gateway/model-id> (-p <text> | --prompt-file <path> | stdin)`. Exactly one source. `--prompt-file` is read once as UTF-8, verbatim: no templating, no wrapping. Reject a missing file, a directory, empty input, invalid UTF-8, and oversized input (documented cap). `-` is a file name like any other. stdin is used only when neither `-p` nor `--prompt-file` is given and stdin is not a TTY.
- [ ] `--headless`: `--prompt-file` is required, `-p` and stdin are rejected, the working directory's `ahi.tools.yaml` is never read (tools come from `--tools` or nothing), every `ask` rule answers deny, and `--result-file` is required. A worker is placed by files, so a prompt in argv or on stdin is a usage error, not sugar.
- [ ] `--prompt-sha256 <hex>`: optional; the run fails with exit 2 if the prompt file's digest differs. Files placed through a shell session can arrive truncated while still being valid UTF-8, and a truncated prompt is a different task. hc computes the digest before placement.
- [ ] `--model <gateway/model-id>` is required. Resolve through the curated `models.json` and the global directory flags: no picker, no TUI default, no mutable last-selection fallback. Actionable errors for a missing file, unknown gateway or model, and missing credential. Never trigger browser login, catalog refresh, or any interactive setup; a missing or invalid credential points at `ahi login`. An expired browser credential may be refreshed non-interactively (existing behavior, [OpenAI inference](OPENAI_INFERENCE.md)); a failed refresh ends the turn with the login guidance and exit 4.
- [ ] `--thinking <off|low|medium|high>`, default off (omit reasoning entirely, the TUI's Off). A provider rejection of the level is a turn failure, not a silent downgrade.
- [ ] `--run-id <id>`: optional caller-chosen identifier (1-64 characters, the same alphabet as rule ids). Echoed into the result file, the history header, every audit line, and every progress line, so hc can join the three. Without it ahi generates one and reports it in the result file.
- [ ] `--check`: preflight. Validate `--tools`, canonicalize `--workdir`, resolve the model and credential, verify the operator files are outside the working directory, write nothing, make no model call, exit 0 or 2..5. hc runs it once per host before dispatching; a human runs it before trusting a new policy file.

### Working directory and tools

- [ ] `--workdir <dir>` (default: the process's current directory) is the worker's working directory: the root for every builtin tool, for `run_command`'s start directory, and for policy rule paths. It must exist and be a directory. Containment is the existing rule (`Workspace::resolve`): the canonical path must equal the lexical join, so absolute paths, `..`, symbolic links, and case variants are refused, and a rule's `path` narrows access and never widens it.
- [ ] `--tools <policy-file>` is the worker's contract, and it can differ per task. With it, the file must exist and parse (as for `ahi tui --tools`), or the run exits 5 before any model call. Headless without `--tools`: no tool is advertised. Interactive without `--tools`: the working directory's `ahi.tools.yaml`, as `ahi tui` does.
  - [ ] `ask` headless. Today a `Verdict::Ask` runs only through an operator decision on the `ApprovalReply` channel or a session grant created by one (`tool_loop.rs`), and grants live in process memory only, so there is no second path to approve an `ask` headless. The implementation must answer every pending call with `ToolDecision::Deny` carrying the reason `requires approval; none available headless`, which the model receives as the tool result. It must **not** drop the reply: a dropped reply cancels the whole batch, including calls a rule alone approved, and ends the turn with an error. Never auto-approve an `ask`.
  - [ ] A policy that parses but approves nothing (`deny-all.yaml`) loads as "no tools" today, and a model that then requests a call fails the turn with `tool execution is disabled` (`http.rs`). Decide whether headless keeps that (exit 1) or treats it as a counted denial. Proposed: keep it; a worker given a policy that can approve nothing and a model that wants tools is a task-authoring error worth surfacing.
  - [ ] A denied call is not a run failure: the model sees it and continues. The result file counts it, with `ask`-denied-headless counted separately from policy-denied, since that is the signal a per-task policy was too narrow.
  - [ ] Command rules run on the host with the operator's access and are not a sandbox (`run_command` spawns with `env_clear` plus the scrubbed base set and the rule's `toolchain_env`, stdin null, no shell). A coordinator's environment variables do not reach the child; the files and flags are the only inputs. Run the worker inside `ahi dev run` for a container boundary. See [Tool calls](TOOL_CALLS.md).

### Operator files are outside the working directory

- [ ] The prompt, history, result, and `--tools` files must not lie inside `--workdir`; exit 2 if any canonicalized path does. Today only *writes* refuse the protected names (`PROTECTED` is checked in `prepare` for `edit_file` and `write_file` only); `read_file`, `search_files`, `find_files`, and `list_dir` resolve anything inside the root that is not an ahi directory, and an approved `run_command` can read or copy any file on the host. Rather than invent a read-side exclusion that `run_command` would bypass anyway, keep the model's files and the operator's files in different trees. hc places the task directory next to the checkout, not in it.

### Continuity: one durable session format

The history file is not a serialized transcript rewritten after each turn. It is an append-only session log, and the same format records `ahi tui` sessions, which today are lost on exit ([Chat screen](CHAT_SCREEN.md)). The shape is borrowed from Pi's session store, inspected locally at `~/.pi/agent/sessions/<cwd-slug>/<timestamp>_<uuid>.jsonl` (Pi 1.4.0): one header line, then one entry per line, each with an `id` and a `parentId`, so a session is a tree whose branches are plain entries sharing a parent, grouped on disk by working directory.

- [ ] **File**: JSONL, UTF-8, one JSON object per line, never rewritten. Header line: `{"type":"session","schema":1,"id":"<uuid>","started":"<rfc3339>","host":"<hostname>","cwd":"<workdir>","gateway":"...","model":"...","ahi_version":"..."}`. Entry lines carry `type`, `id` (8 hex chars from the OS, unique in the file), `parentId` (null for the first), `timestamp`, and the type's fields:
  - `message`: `role` (`operator`, `assistant`, `tool`), `text`, and when present the structured `ToolPayload` (calls, or one result with `call_id`, `name`, `content`) and the provider's `NativeReplay` as an opaque `native` field tagged with its protocol. Screen-only notices are not entries.
  - `turn`: one per `ahi exec` invocation or chat turn: `run_id`, `status`, `exit_code`, `usage`, `effects`, and the `ledger`. This is the line a reader uses to answer "what did that turn do" without replaying the messages.
  - `model_change`, `thinking_change`: as in the chat's `/model` and `/thinking`.
- [ ] **Tip, not rewrite.** A turn appends its entries under the current tip. A follow-up appends under the last `turn`. A failed turn that ran tools appends what ran and a `turn` with `status: failed`; the next attempt appends under the last *successful* entry, so the failed branch stays on disk and is simply not the active path. Nothing is edited or removed, so "history unchanged on failure" is replaced by one rule: every line that was written stays, and the reader follows parents from the newest `turn` to the root. The old claim "unchanged on failure, so retrying is safe" is withdrawn either way: the tool loop fails turns *after* effects (the round cap, a cancelled batch, and any failed continuation all return after the round's calls executed), so a retry runs against a changed checkout and the ledger on the failed `turn` line is how a coordinator knows what changed.
- [ ] **Concurrency** is the tip check: the worker records the tip `id` at read and appends only if the file still ends there; otherwise exit 7, no lock files. Two workers on one file cannot both win.
- [ ] `--history-file <path>` names the file for a worker; hc owns the path and ferries the file. Without it, `ahi exec` writes a session under `<state>/sessions/<cwd-slug>/` like the chat, so a one-shot human turn is as durable as a chat turn. `--no-history` disables the write for hermetic runs.
- [ ] **Compatibility**: a header `schema` this ahi cannot read is exit 7, never a silent restart. `NativeReplay` is replayed only to the protocol that produced it (as the TUI does) and a history whose header names another gateway or model is exit 7 unless the caller passes `--history-model-change`, which appends a `model_change` and drops native items that cannot cross; nothing is dropped silently.
- [ ] **Not borrowed from Pi**: the durable harness `@earendil-works/pi-durable` (1.5.2 upstream, not part of the installed 1.4.0) goes further: a storage engine (SQLite or JSONL) that commits conversation entries, task checkpoints, and documents in one transaction, resumes interrupted tasks on reopen, and lets a tool declare itself replay-safe so a crash mid-tool can rerun it (`replay?: "never" | "safe"` in the installed `pi-agent-core` types; `"safe" | "unsafe"` in the current upstream spec). Three of its ideas are adopted here without the engine: **intent before effect** (a `tool` entry's call line is appended before the tool runs, its result line after, so a crash leaves a visible unmatched call, which is what `effects: unknown` means on disk); **a request id for idempotent resubmission** (`run_id` plays that role: the same `run_id` appended twice is refused with exit 7); and **no cross-process locking** (one writer per file, detected by the tip check). Not adopted: in-process task scheduling, SQLite, documents, or automatic rerun of replay-safe tools; a worker is one turn and the coordinator decides whether to try again.

### Result

- [ ] `--result-file <path>` receives one versioned JSON object, written atomically on every outcome, including failures before the model call when the path is writable:

  ```json
  {
    "schema": 1, "ahi_version": "0.14.1", "run_id": "...",
    "status": "ok" | "failed" | "cancelled" | "timeout",
    "gateway": "...", "model": "...",
    "text": "...", "text_truncated": false,
    "error": {"kind": "...", "message": "..."},
    "usage": {...},
    "tools": {"calls": 0, "denied": 0, "ask_denied": 0, "invalid": 0, "rounds": 0},
    "effects": "none" | "some" | "unknown",
    "ledger": [{"tool": "...", "target": "...", "outcome": "ok", "before": "sha256", "after": "sha256"},
               {"tool": "run_command", "argv": ["..."], "cwd": "...", "exit_code": 0}],
    "history": {"status": "appended" | "write_failed" | "none", "session": "<uuid>", "tip": "<entry id>"}
  }
  ```

  `text` appears only for `ok` and is capped (documented; `text_truncated` says so). `usage` only when the provider reported it. `ledger` holds the same fields the audit events already record (paths and hashes, argv and exit codes; never contents or output). `effects` is `none` when no write or command ran, `some` when the ledger is complete, `unknown` when the run ended with a call in flight. A coordinator reads one file and the exit code; stderr is diagnostics, never an interface.
- [ ] The result file must not already exist; exit 2 if it does. A stale result from a previous run of the same task is the easiest way for a coordinator to read a success that did not happen, and `write_file` already refuses to clobber without `overwrite` for the same reason. hc removes the old one before placement.
- [ ] Without `--result-file` (interactive), `--format text` (default) prints the final answer on stdout and `--format json` prints the same object.

### Limits

- [ ] `--timeout <secs>`, documented default, for the whole run. Implementation: `recv_timeout` on the response receiver, then `CompletionBackend::cancel()`. Today `cancel` is a flag checked between rounds and in the stream reader; it does not interrupt a running `run_command` (own timeout, default 120 s, max 600 s) or a long search, and a write can still land after the deadline. So the deadline is also passed into `ToolRuntime`: a `run_command` clamps its timeout to the remaining budget, and a call that would start past the deadline is denied with `deadline reached`. Even so, document that `status: timeout` may carry `effects: unknown` when a call was running at the deadline. A timeout is never retried by ahi.
- [ ] Tool rounds: the existing 8-round, 256 KiB-per-turn limits. Expose as flags only if hc needs them.
- [ ] Gateway connection failures (DNS, refused, reset) are retried a bounded number of times with backoff before any byte reaches the server (`Reconnecting`, `http.rs`); the headless loop drains those events and they count against `--timeout`. 429, 5xx, malformed streams, refusals, and truncation are failures, exit 1, not retried; rate-limit handling is the coordinator's.

## Output, progress, exit codes, signals

- [ ] stdout carries only the final answer (or the JSON object), buffered until a validated successful terminal event, written once. A failed, refused, truncated, cancelled, or timed-out run leaves nothing on stdout that could be mistaken for an answer. UTF-8 regardless of locale, no ANSI.
- [ ] Progress goes to stderr, one bounded line per tool round in headless mode: `run_id`, round number, `tool target`, exit code, elapsed; never prompt text, arguments, patterns, or output. hc shows the session, so this is what the human sees for a long run; a result-file reader ignores it. Other diagnostics also go to stderr, sanitized as today: never secrets, upstream bodies, or token values. Interactive mode prints the same lines only with `-v`.
- [ ] Broken pipe: Rust ignores SIGPIPE, so a closed stdout surfaces as EPIPE. Exit quietly with the defined code; never panic, never treat it as a model failure.
- [ ] Signals: on SIGINT, SIGTERM, and SIGHUP (the ssh session dropped), cancel, write the result file with `cancelled` and the ledger so far, and exit 6. SIGHUP matters most: hc reconnects and expects to find a result file.
- [ ] Exit codes (proposed; finalize with hc):

| Code | Meaning |
| --- | --- |
| 0 | Validated successful completion |
| 1 | Turn failure (transport, malformed stream, refusal, truncation, tools disabled) |
| 2 | Usage error (bad arguments, unreadable or invalid prompt file, digest mismatch, bad `--workdir`, operator file inside the workdir, result file already exists) |
| 3 | Configuration or curation error (missing or invalid `models.json`, unknown gateway or model) |
| 4 | Credential missing or refresh failed; `ahi login` required |
| 5 | Policy file missing or invalid (fail closed before any model call) |
| 6 | Timeout or cancellation (signal) |
| 7 | History file unreadable, unsupported schema, wrong gateway/model, or changed during the run |
| 8 | Result file could not be written after the turn; the outcome is on stderr only |
| 255 | Not ahi: ssh's own failure code. hc treats it as "session lost" |

- [ ] A missing result file with any exit code, a code outside this table (SIGKILL, panic, OOM: 101, 137, …), or 255 means `effects: unknown` to a coordinator. Say so in the hc contract: an absent file is never "not started".

## `ahi history`: reading sessions, here and across machines

The session format is useful only if something reads it. ahi reads locally; hc reads across machines by running ahi where the files are, so no new protocol or index exists.

- [ ] `ahi history list [--cwd <dir>] [--since <rfc3339|duration>] [--json]`: one row per session from headers and the last `turn` line only (first and last lines of each file): session id, started, host, cwd, gateway/model, turns, last status, last `run_id`. `--json` prints one object per line for a coordinator.
- [ ] `ahi history show <session-id|path> [--json] [--branch <entry id>]`: the active path from the tip to the root as text (the chat's plain rendering, no ANSI) or as the raw entries. `--branch` shows a non-active branch, which is how a failed attempt is inspected.
- [ ] `ahi history grep <pattern> [--cwd <dir>] [--json]`: regular expression over message text, tool targets, and argv across every session under `<state>/sessions/`, printed as `<session id>:<entry id>: <line>` in the `search_files` style. Prompt and answer text are in these files by nature, so the command reads only the owner-only state directory and never a path the model can reach.
- [ ] `ahi history export <session-id> --to <path>`: copy one file; the file is already the interchange format, so this is a convenience with a refusal for an existing destination.
- [ ] **Across machines is hc's job and costs hc nothing new.** hc already has a shell on every host: `ssh <host> ahi history list --json --since 1d` merged across hosts is the fleet view, and every row carries `host` and `cwd`, so merged rows are attributable. Worker histories hc placed itself are already local to hc. If hc wants them in one place, the directory is plain files with UUID names that never collide: `rsync` the `sessions/` directories and run `ahi history` once. No daemon, no database, no sync protocol in ahi.
- [ ] **Across chats** is `ahi history grep` over one directory. A `delegate` result ([Delegation](DELEGATION.md)) records the worker's session id in the main session's `turn` line, so a main-session entry links to the worker file it came from, and `ahi history show` follows that link when the file is present.
- [ ] Reading is `std::fs` and `serde_json`, line by line; nothing is loaded whole except the one session `show` prints. A file whose header schema is newer than this ahi is listed with `schema: unsupported` and skipped by `grep` and `show`, never an error for the whole listing.

## Implementation

- [ ] `src/main.rs`: add an `Exec` subcommand; every existing subcommand, global flag, and help or error path stays unchanged. `--color` is accepted and ignored (no ANSI on this path). Proposed module `src/chat/exec.rs`, keeping terminal modules untouched.
- [ ] Turn loop: build the `ChatRequest` (selection, thinking, history plus the prompt as one user message), construct the same `HttpBackend` the TUI uses with `--workdir` as the tool root and `--tools` as the explicit policy, drain the `Receiver<ResponseEvent>`: accumulate `TextDelta`, capture `Usage` and `ContextWindow`, record every `ToolResult` into the ledger, drain `Reconnecting`, answer `ToolApprovalRequested` with per-call `Deny` headless or with a terminal prompt interactively (reusing the approval reply channel, never a new path), and treat `Completed` as the only success. Map `Failed { kind, .. }` to exit codes (`Auth` → 4, `Policy` for a bad policy file → 5, the rest → 1). Preserve every boundary from [OpenAI inference](OPENAI_INFERENCE.md) and [Providers](PROVIDERS.md).
- [ ] No terminal initialization, no crossterm, no TUI event loop in headless mode; stdin is never read, so the process works with stdin closed, stdin a pty, and stdout fully redirected. `run_command` children already get `Stdio::null()` for stdin.
- [ ] Audit: writes and commands already append hash-only and argv-only lines to `audit.log`, but both events hard-code `"command": "tool"` and carry no run identity. Add `"origin": "exec"` and `"run_id"` to `WriteEvent` and `CommandEvent`; the TUI records `"origin": "tui"`. Audit appends are best-effort today (a failed append does not undo a write); the result ledger is the reliable record.
- [ ] Side effects of a normal run: credential rotation on refresh (atomic replacement), tool effects the policy approved, and the files the caller named. Honor the global `--state-dir`, `--cache-dir`, and `--config` flags so hc can run workers hermetically with disposable state. No logs by default; any logging must never contain prompt or answer text.
- [ ] Concurrency: parallel `ahi exec` processes are safe (no shared mutable state, no lock files) except for two runs sharing one history file, which the changed-during-run check catches, and two runs sharing one `--workdir`, where `edit_file` races produce wrong edits rather than errors. hc gives each task its own checkout (`git worktree` fits); document it.

## Verification and rollout

- [ ] Integration tests with disposable state and the loopback mock gateway (the pattern from [OpenAI inference](OPENAI_INFERENCE.md)); no real credentials, no external network. Cover:
  - input: `-p`, stdin, and `--prompt-file`; exactly-one enforcement; headless rejecting `-p` and stdin; missing, empty, oversized, and invalid files; digest mismatch; stdin closed, a pipe, and a pty.
  - models and credentials: resolution and curation errors, missing credential, `--check`.
  - output: text and JSON success; result file on every status including pre-model failures; refusing an existing result file; stdout empty on failure; progress lines on stderr with no argument text; Unicode and multibyte fragmentation; EPIPE; no-terminal operation.
  - tools and workdir: headless with no `--tools` advertises nothing; `deny-all` turn semantics; `ask` answered as per-call deny with the reason and the batch's rule-approved calls still running; approved read, edit, and command rules; workdir escapes, `..`, symlinks, and case variants refused; `run_command` starts inside the workdir with no coordinator environment; operator files inside the workdir refused at startup; the ledger matching the audit log.
  - continuity: a follow-up appends under the last `turn` and the model sees the earlier messages; a failed turn with effects appends its calls, results, and a failed `turn`, and the next attempt branches from the last success; a crash between a call line and its result line (simulated by killing the binary while a `run_command` sleeps) leaves the unmatched call and `history list` reports the session's last status as `unknown`; tip check exit 7 when a second writer appended; unsupported schema exit 7; gateway/model mismatch exit 7 and the `--history-model-change` path; a duplicate `run_id` refused.
  - history commands: `list`, `show` (active path and `--branch`), `grep`, `export` over a fixture directory holding sessions from two "hosts" and one with a newer schema; `--json` rows parse; nothing outside the state directory is read.
  - lifecycle: malformed or incomplete streams, refusal, truncation, `Reconnecting`, timeout with a running command (`effects: unknown`), SIGTERM and SIGHUP writing `cancelled`.
- [ ] Regression: existing subcommands, TUI paths, and the clap help and version surface; the repository's normal format, lint, and test checks; Windows behavior (no SIGPIPE, no SIGHUP) alongside Unix.
- [ ] Rollout in slices, each shippable and testable against the mock gateway:
  1. `ahi exec` with `-p`, stdin, `--prompt-file`, `--model`, `--thinking`, text and JSON output, `--result-file` with the schema above, exit codes 0..4 and 8, no tools. A human can use it the day it lands.
  2. `--workdir`, `--tools`, `--headless`, `ask` as deny, operator-files-outside check, the ledger and `effects`, exit 5, `--check`, progress on stderr, audit `origin`/`run_id`.
  3. `--timeout`, signals, deadline plumbed into `ToolRuntime`, exit 6. Signal handling needs `sigaction`, which `std` does not expose; per the repository's dependency policy that is an in-repo workspace crate holding the one `unsafe` call (as `ahi-win32` does for the clipboard), not a registry crate, and `ahi` itself keeps `forbid(unsafe_code)`.
  4. The session format: the writer (`src/chat/session.rs`) used by both `ahi exec` (`--history-file`, default `<state>/sessions/`, `--no-history`) and `ahi tui` (every turn appended; `ahi tui --resume <id>` reopens one), the tip check, exit 7, and `ahi history list|show|grep|export`. Slice 4a ships the writer and `exec`; 4b adds the chat and `--resume`; 4c adds `history grep` and `export`. Each is independently testable on fixture files.

  Then agree the JSON schemas, exit codes, and timeout semantics with hc and add an hc-side integration test that launches the real binary in a shell against the mock gateway. Document invocation, security, limits, and exit codes in [`README.md`](../README.md) (Commands table and a section) and record each slice in [`CHANGELOG.md`](../CHANGELOG.md).

## Later, not in this plan

- Delegation from a main `ahi tui` session: a `delegate` builtin tool governed by `action: delegate` rules naming permitted worker labels. The tool hands hc a task (hc as a program on PATH, argv only, result file back; no library, no socket) and returns the worker's result `text` and `effects` as the tool result; hc places files, picks the host, and keeps the history. ahi still never chooses a host. This is the designed path, not a maybe; it gets its own document (`docs/DELEGATION.md`) once hc's command surface is known.
- Steering a running worker (beep-boop's `message_task`) is an hc feature over its session; a worker is one turn. Follow-ups are new turns appended to the session file.
- An events file or JSONL stream beyond the stderr progress lines, if hc wants structure rather than text. The session file is append-only, so `tail -f` on it is already a progress stream; a separate events file exists only if hc needs one before the turn's lines are written.
- A pi-durable style resumable task engine (checkpointed tool tasks, automatic rerun of replay-safe tools, documents beside the transcript). The session format leaves room for it (an unmatched call line is the checkpoint), but the coordinator decides on retry today, and ahi does not.
- Workers without hc (`ahi.workers.yaml` with ssh targets). Only if a no-hc use case appears.

## Open questions

- Which four primitives does hc actually expose for a worker host: place a file, run a command in a session, read a file back, read the exit status? And is placement `scp`/`tailscale file cp` or a heredoc in the pty? The digest option exists because the answer may be the latter.
- Should the result file's `text` cap be a flag or fixed? Proposed: fixed, generous, with `text_truncated`.
- Should `--tools` accept the shipped example names (`--tools read-all`) as well as paths? Proposed: paths only; `ahi tools init --example` already writes a file to start from.
- Should `ahi tui` start writing sessions by default in slice 4b, or behind a flag first? The directory is owner-only and the chat already sends the same text to a provider, so the privacy posture is unchanged; the cost is disk. Proposed: default on, `--no-history` to opt out, and `ahi history` is how the human sees what is kept.
- Should `history grep` read worker sessions hc placed outside `<state>/sessions/`? Proposed: only through an explicit `--path`, never by discovery.

## Decisions taken in this revision

1. Operator files live outside `--workdir` and the worker refuses otherwise, instead of a new read-side exclusion mechanism.
2. The result file carries a ledger and an `effects` field on every outcome, and history is rewritten after a failed turn that ran tools; "safe to retry" is withdrawn.
3. `--timeout` is enforced inside the tool loop as far as the code allows (deadline plumbed to `ToolRuntime`, `run_command` clamped), and `status: timeout` may still report `effects: unknown`.
4. History is an append-only JSONL session tree shared by `ahi exec` and `ahi tui`, modeled on Pi's session files, with a `turn` line per invocation and the call line written before the tool runs. Pi's durable engine is noted and not adopted. "Inspectable or opaque" is answered: inspectable, and `ahi history` is the reader; hc reads across machines by running it over ssh.
