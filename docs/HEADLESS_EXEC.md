# One-shot turns and headless workers (`ahi exec`)

Status: slices 1 and 2 are implemented (`ahi exec` with `-p`, stdin, `--prompt-file`, `--prompt-sha256`, `--model`, `--thinking`, `--format`, `--result-file`, `--headless`, `--run-id`, `--workdir`, `--tools`, `--check`, `-v`; the ledger and `effects`; audit `origin`/`run_id`; exit codes 0..5 and 8). Timeouts, signals, and the durable store (slices 3 and 4) are design for review: checked boxes are shipped, unchecked ones are proposed syntax, not commands available today.

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
- [ ] **Intent before effect.** A `tool` call line is appended before the tool runs and its result line after, so a crash leaves a visible unmatched call. That unmatched call is what `effects: unknown` means on disk, and it is the checkpoint the durable store below resumes from.

### Durable store: pi-durable's model, two backends

The JSONL file above is the interchange format: hc ferries it, `rsync` syncs it, a human reads it with `less`. It is not the best local store once there are hundreds of sessions, and it cannot commit a conversation entry and a task checkpoint in one step. Pi solves this in `@earendil-works/pi-durable` (1.5.2 upstream; not part of the installed Pi 1.4.0, so there is no local engine to call), and its model is adopted here as a whole rather than piecemeal: **one logical store, pluggable backends**. pi-durable ships SQLite, JSONL, and in-memory backends behind one storage interface; ahi does the same, with the JSONL backend being the file format above and the SQLite backend being the default local store.

The logical model, as pi-durable defines it and as ahi keeps it:

| Record | What it holds | Where in JSONL | Where in SQLite |
| --- | --- | --- | --- |
| conversation | the header: id, started, host, cwd, gateway, model, schema, current tip | header line | `conversations` row |
| entry | `message`, `turn`, `model_change`, `thinking_change`, with `id`/`parentId` | one line each | `entries` row (`conversation_id`, `id`, `parent_id`, `seq`, `type`, `body`) |
| submission | a request id and the stored outcome of the turn it started | the `turn` line's `run_id` | `submissions` row keyed by `run_id`, holding the result object |
| task | one turn's tool plan and which calls completed, per tool `replay` policy | the unmatched call lines | `tasks` row with a `checkpoint` JSON and `status: open \| done \| abandoned` |
| document | a named, versioned blob kept beside the transcript (a plan, a diff the model is working from) | not representable; export writes `documents/<name>.<version>` next to the file | `documents` row (`name`, `version`, `sha256`, `body`) |

- [ ] **One trait, two backends.** `src/chat/session/store.rs` defines `SessionStore` with `open`, `append(entries, task_checkpoint, submission)` as one atomic step, `tip`, `read_path`, `list`, `find_submission(run_id)`, `open_tasks`. `jsonl.rs` implements it over the file format (atomicity is one `write_all` of the concatenated lines after the tip check; a checkpoint is the call line). `sqlite.rs` implements it over one database at `<state>/sessions.db` with one transaction per `append`. Tests run the same suite against both; the in-memory backend is the SQLite backend on `:memory:`.
- [ ] **Which backend when.** `--history-file <path>` selects JSONL at that path (hc owns the path and ferries the file). Without it, `ahi exec` and `ahi tui` use SQLite under `<state>/` so a one-shot human turn and a chat turn land in the same queryable store. `--no-history` selects no store. The two never mix inside one session; `ahi session export` and `ahi session import` convert between them, and a session that arrived as a file from another host can be imported so `grep` and `list` see it.
- [ ] **Idempotent submission.** pi-durable's request id is `run_id`. A second `ahi exec` with a `run_id` the store already holds and the same `--prompt-sha256` returns the stored result object, rewrites the result file, and exits with the stored exit code; it does not call the model. The same `run_id` with a different prompt is exit 7. This is what makes an hc retry after a lost ssh session safe: the worst case is the coordinator reading the same answer twice.
- [ ] **Tasks and replay.** A tool declares a replay policy in the builtin table (`replay: safe` for `read_file`, `search_files`, `list_files`; `replay: never` for `write_file`, `edit_file`, `run_command`; the installed `pi-agent-core` types spell it `"never" | "safe"`, upstream `"safe" | "unsafe"`, and ahi uses the installed spelling). Opening a session with an open task (a crash left unmatched calls) reruns the replay-safe calls, records the others as `outcome: unknown` in the ledger, and continues the turn from there; `ahi exec` does this when resubmitted with the same `run_id`, `ahi session --resume` does it on open. Nothing with `replay: never` is ever rerun by ahi; the coordinator decides.
- [ ] **One writer.** Still no lock files. SQLite's own transaction lock replaces the tip check inside the database; a second writer on the same session id gets `SQLITE_BUSY` after the configured timeout and exits 7, the same code as the JSONL tip check. Two processes may share one database on different sessions.
- [ ] **Documents** ship last and only for the chat: `/doc save <name>` and `/doc show <name>` keep a versioned blob the model is working from without stuffing it into every request; `ahi exec` does not use them in this plan.
- [ ] **The dependency.** SQLite is C. The repository forbids `unsafe` in `ahi` and avoids new registry crates, and `rusqlite` plus `libsqlite3-sys` plus a bundled amalgamation is exactly the shape that rule exists to avoid. `sqlx` was considered and is a worse fit: it is async-first and brings a runtime (tokio or async-std) into a crate that has none, its SQLite driver still rests on `libsqlite3-sys`, its tree is an order of magnitude larger than rusqlite's, and its compile-time query checking wants a database at build time. [Turso](https://github.com/tursodatabase/turso) (`turso` 0.8.3, MIT, a SQLite re-implementation in Rust tracking 3.50.4) was measured in a scratch crate on 2026-11-11 and is the credible alternative: with `default-features = false` there is no tokio, a twelve-line `std` `block_on` drives its futures, `BEGIN`/`COMMIT` and `UPSERT` work, the resulting file opens in the `sqlite3` CLI, and Windows is supported, so it closes the Windows gap with no C at all. Against it: it adds 128 crates that ahi does not build today (ahi builds 129 in total, so the tree roughly doubles), `turso_core` contains several hundred `unsafe` sites, it is pre-1.0 with experimental subsystems, and its compatibility notes say mixed SQLite and Turso access to one file across processes is unsupported, which bears on a human running `sqlite3` on the store while `ahi tui` is open. **Decision (2026-11-11): Turso, wrapped.** One backend on every platform from day one, no C compiler or system library in the build, and a file that stays SQLite-readable outweigh the crate count. The wrapper follows the `ahi-win32` pattern in shape, not in substance: a workspace crate `crates/ahi-store-turso` is the only member that names `turso` (pinned exact, `default-features = false`), holds the `block_on`, and exports the synchronous `SessionStore` implementation; `ahi` sees no futures and no Turso types. What the wrapper buys is one owner for the dependency, a crate-level lint boundary (`ahi`, `ahi-widgets`, and the wrapper's own code stay `forbid(unsafe_code)`; the dependency is the named exception), and a swap seam. What it does not buy is a smaller review surface: the 128 crates and `turso_core`'s `unsafe` come along regardless, and are accepted knowingly. Rules that follow: Turso upgrades are deliberate, reviewed bumps while it is pre-1.0; the store is read with `ahi session`, never with a concurrent `sqlite3` process while a session is open (documented in `README.md` and in `ahi session --help`); and the fallback, if Turso's churn or the multi-process caveat bites, is the in-repo FFI crate against the system `libsqlite3` behind the same trait (see Later).

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

## `ahi session`: resuming and reading sessions, here and across machines

The store is useful only if something reads it. ahi reads locally from whichever backend holds the session; hc reads across machines by running ahi where the data is, so no new protocol exists.

- [ ] `ahi session --resume <uuid>`: open the chat on an existing session, from the SQLite store by default or from a JSONL file given as a path. The transcript is rebuilt from the active path, an open task is replayed as the durable store section says, and the header's gateway and model are selected unless `--model` overrides them (which appends a `model_change`). `ahi tui --resume <uuid>` is an alias so the verb lives where the screen is.
- [ ] `ahi session list [--cwd <dir>] [--since <rfc3339|duration>] [--json]`: one row per session: id, started, host, cwd, gateway/model, turns, last status, last `run_id`, backend. SQLite answers from the `conversations` table; JSONL answers from each file's first and last lines only. `--json` prints one object per line for a coordinator.
- [ ] `ahi session show <uuid|path> [--json] [--branch <entry id>]`: the active path from the tip to the root as text (the chat's plain rendering, no ANSI) or as raw entries. `--branch` shows a non-active branch, which is how a failed attempt is inspected.
- [ ] `ahi session grep <pattern> [--cwd <dir>] [--json]`: regular expression over message text, tool targets, and argv across every session in the store, printed as `<session id>:<entry id>: <line>` in the `search_files` style. SQLite narrows by `type` and `cwd` in SQL and applies the regex in Rust; JSONL scans files line by line. Prompt and answer text are in the store by nature, so the command reads only the owner-only state directory and never a path the model can reach.
- [ ] `ahi session export <uuid> --to <path>` and `ahi session import <path>`: convert between backends. Export writes the JSONL file (and `documents/` beside it when any exist) and refuses an existing destination; import appends a file's session into SQLite and refuses a session id already present unless the file's tip extends the stored one, which is how a worker's follow-up turns arrive.
- [ ] **Across machines is hc's job and costs hc nothing new.** hc already has a shell on every host: `ssh <host> ahi session list --json --since 1d` merged across hosts is the fleet view, and every row carries `host` and `cwd`, so merged rows are attributable. Worker sessions hc placed itself as `--history-file` are already local to hc, and `ahi session import` on the hc host puts them in one queryable store. If hc wants the whole fleet in one place, `rsync` the JSONL and import; UUID names never collide. No daemon and no sync protocol in ahi, and the database is never shared across hosts (SQLite over a network filesystem is the documented way to corrupt one).
- [ ] **Across chats** is `ahi session grep` over one store. A `delegate` result ([Delegation](DELEGATION.md)) records the worker's session id in the main session's `turn` line, so a main-session entry links to the worker session it came from, and `ahi session show` follows that link when the session is present locally.
- [ ] Reading is `std::fs` and `serde_json` for JSONL, and the in-repo SQLite bindings for the database; nothing is loaded whole except the one session `show` prints. A session whose schema is newer than this ahi is listed with `schema: unsupported` and skipped by `grep` and `show`, never an error for the whole listing.

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
  - continuity: a follow-up appends under the last `turn` and the model sees the earlier messages; a failed turn with effects appends its calls, results, and a failed `turn`, and the next attempt branches from the last success; a crash between a call line and its result line (simulated by killing the binary while a `run_command` sleeps) leaves the unmatched call and `session list` reports the session's last status as `unknown`; tip check exit 7 when a second writer appended; unsupported schema exit 7; gateway/model mismatch exit 7 and the `--history-model-change` path; a duplicate `run_id` with a different prompt digest refused.
  - store: the same `SessionStore` suite against JSONL, SQLite on disk, and SQLite `:memory:`: append is atomic (kill between entry and checkpoint leaves neither), `find_submission` returns the stored result for a known `run_id`, a resubmission with the same `run_id` and prompt digest skips the model and rewrites the result file, a different digest is exit 7, an open task reruns only `replay: safe` calls and marks the rest `unknown`, two writers on one session get exit 7 from both backends, two writers on different sessions in one database both succeed, `export` then `import` round-trips byte-identical entries, and the suite runs on Windows CI with the same backend set as Unix.
  - session commands: `--resume` on both backends (transcript rebuilt, open task replayed, `--model` appends `model_change`), `list`, `show` (active path and `--branch`), `grep`, `export`, `import` over fixtures holding sessions from two "hosts" and one with a newer schema; `--json` rows parse; nothing outside the state directory is read.
  - lifecycle: malformed or incomplete streams, refusal, truncation, `Reconnecting`, timeout with a running command (`effects: unknown`), SIGTERM and SIGHUP writing `cancelled`.
- [ ] Regression: existing subcommands, TUI paths, and the clap help and version surface; the repository's normal format, lint, and test checks; Windows behavior (no SIGPIPE, no SIGHUP) alongside Unix.
- [ ] Rollout in slices, each shippable and testable against the mock gateway:
  1. `ahi exec` with `-p`, stdin, `--prompt-file`, `--model`, `--thinking`, text and JSON output, `--result-file` with the schema above, exit codes 0..4 and 8, no tools. A human can use it the day it lands.
  2. `--workdir`, `--tools`, `--headless`, `ask` as deny, operator-files-outside check, the ledger and `effects`, exit 5, `--check`, progress on stderr, audit `origin`/`run_id`.
  3. `--timeout`, signals, deadline plumbed into `ToolRuntime`, exit 6. Signal handling needs `sigaction`, which `std` does not expose; per the repository's dependency policy that is an in-repo workspace crate holding the one `unsafe` call (as `ahi-win32` does for the clipboard), not a registry crate, and `ahi` itself keeps `forbid(unsafe_code)`.
  4. The durable store, in five independently shippable parts:
     - **4a** `SessionStore` trait and the JSONL backend (`src/chat/session/{mod,store,jsonl}.rs`), used by `ahi exec` for `--history-file` and, until 4b lands, for the default `<state>/sessions/` location; the tip check; exit 7; the call-before-result discipline. Fixture-file tests only.
     - **4b** `crates/ahi-store-turso` (the wrapper crate: pinned `turso`, no default features, the `block_on`, the `SessionStore` impl) and the SQLite backend with `conversations`, `entries`, `submissions`, `tasks`; it becomes the default store on every platform; idempotent `run_id` resubmission in `ahi exec`. This slice gets its own PR and review because it is the one that adds the dependency tree.
     - **4c** `ahi tui` appends every turn; `ahi session --resume <uuid>` (alias `ahi tui --resume`); open-task replay for `replay: safe` tools.
     - **4d** `ahi session list|show|grep|export|import`.
     - **4e** `documents` and the chat's `/doc` commands; optional, last, and only if 4c shows a need.

  Then agree the JSON schemas, exit codes, and timeout semantics with hc and add an hc-side integration test that launches the real binary in a shell against the mock gateway. Document invocation, security, limits, and exit codes in [`README.md`](../README.md) (Commands table and a section) and record each slice in [`CHANGELOG.md`](../CHANGELOG.md).

## Later, not in this plan

- Delegation from a main `ahi tui` session: a `delegate` builtin tool governed by `action: delegate` rules naming permitted worker labels. The tool hands hc a task (hc as a program on PATH, argv only, result file back; no library, no socket) and returns the worker's result `text` and `effects` as the tool result; hc places files, picks the host, and keeps the history. ahi still never chooses a host. This is the designed path, not a maybe; it gets its own document (`docs/DELEGATION.md`) once hc's command surface is known.
- Steering a running worker (beep-boop's `message_task`) is an hc feature over its session; a worker is one turn. Follow-ups are new turns appended to the session file.
- An events file or JSONL stream beyond the stderr progress lines, if hc wants structure rather than text. The session file is append-only, so `tail -f` on it is already a progress stream; a separate events file exists only if hc needs one before the turn's lines are written.
- The fallback store backend: an in-repo, dependency-free FFI crate `crates/ahi-sqlite` (a dozen `extern "C"` declarations, a safe `Connection`/`Statement` API, linked to the system `libsqlite3`) implementing the same `SessionStore`. Built only if Turso's pre-1.0 churn or its multi-process caveat becomes a problem; the trait keeps the swap local to one crate.
- Full-text search for `ahi session grep`. The regex scan is enough until a store is large enough to show otherwise; Turso's own FTS is experimental and SQLite's FTS5 is not supported by Turso, so neither is assumed.
- Workers without hc (`ahi.workers.yaml` with ssh targets). Only if a no-hc use case appears.

## Open questions

- Which four primitives does hc actually expose for a worker host: place a file, run a command in a session, read a file back, read the exit status? And is placement `scp`/`tailscale file cp` or a heredoc in the pty? The digest option exists because the answer may be the latter.
- Should the result file's `text` cap be a flag or fixed? Proposed: fixed, generous, with `text_truncated`.
- Should `--tools` accept the shipped example names (`--tools read-all`) as well as paths? Proposed: paths only; `ahi tools init --example` already writes a file to start from.
- Should `ahi tui` start writing sessions by default in slice 4c, or behind a flag first? The store is owner-only and the chat already sends the same text to a provider, so the privacy posture is unchanged; the cost is disk. Proposed: default on, `--no-history` to opt out, and `ahi session` is how the human sees what is kept.
- Should `session grep` read worker JSONL files hc placed outside `<state>/`? Proposed: only through an explicit `--path` or after `ahi session import`, never by discovery.
- How is a Turso upgrade decided while it is pre-1.0? Proposed: exact pin, bump only with a CHANGELOG entry and the store suite green on all three platforms, and a fixture database written by the previous version opened in the suite so file-format drift is caught.

## Decisions taken in this revision

1. Operator files live outside `--workdir` and the worker refuses otherwise, instead of a new read-side exclusion mechanism.
2. The result file carries a ledger and an `effects` field on every outcome, and history is rewritten after a failed turn that ran tools; "safe to retry" is withdrawn.
3. `--timeout` is enforced inside the tool loop as far as the code allows (deadline plumbed to `ToolRuntime`, `run_command` clamped), and `status: timeout` may still report `effects: unknown`.
4. History is an append-only session tree shared by `ahi exec` and `ahi tui`, modeled on Pi's session files, with a `turn` line per invocation and the call line written before the tool runs. "Inspectable or opaque" is answered: inspectable, and `ahi session` is the reader; hc reads across machines by running it over ssh.
5. pi-durable's model is adopted whole: one `SessionStore` with a JSONL backend (the interchange format, what hc ferries) and a SQLite-format backend (the default local store; one transaction per append; `run_id` as the idempotent submission key; task checkpoints with per-tool `replay` policy). Both are first-class and `export`/`import` convert between them. The database engine is Turso (a Rust SQLite re-implementation, no C), owned by the wrapper crate `crates/ahi-store-turso` so `ahi` stays synchronous and `forbid(unsafe_code)`; `rusqlite`, `sqlx`, and the system-library FFI crate were weighed and the FFI crate is kept as the fallback behind the same trait.
