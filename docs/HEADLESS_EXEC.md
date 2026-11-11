# Headless worker turns (`ahi exec`)

Status: design plan, not implemented. This document is for review; nothing here changes code yet, and the examples below are proposed syntax, not commands available today.

Let a coordinator hand ahi a task as a file and get a result back as a file. The coordinator is herdr-core (`hc`), or a main ahi session that delegates to workers; the workers are `ahi exec` processes on this machine or another one. The shape follows [beep-boop](https://github.com/jessfraz/beep-boop): one coordinator owns routing and task state, workers own a conversation and a working directory, and results return to the coordinator. Division of responsibility here: the coordinator (hc) keeps orchestration, scheduling, and task state; ahi performs one bounded turn per invocation, with any continuity carried in files the caller owns. ahi does not become a coordinator, daemon, or session store, never learns about herdr, and this plan does not assume any hc protocol: confirm hc's subprocess and file requirements before freezing the interface.

```text
coordinator (hc, or a main ahi)            worker machine (local or remote)
  task.md  ───── copy ─────────────────▶   ahi exec --prompt-file task.md
  task.tools.yaml (per task)  ─ copy ──▶        --tools task.tools.yaml
  task.history.json (follow-ups) ─────▶         --history-file task.history.json
                                                --workdir /path/to/checkout
                                                --result-file result.json
  result.json  ◀──── copy ─────────────    exit status 0..7
  task.history.json ◀── copy ──────────
```

Files in, files out, one exit code. Local and remote workers look the same to ahi: only paths cross its boundary, and moving files is the coordinator's job.

## Why a prompt file, and no `-p`

- A prompt for a delegated task is large, multi-line, and often sensitive. A file keeps it out of argv (visible to other processes on the host), out of shell quoting, and out of size limits. The file is the whole interface: `ahi exec` never reads stdin, so there is no pipe, no TTY detection, and no second input source to disambiguate. A caller with text in hand writes a temp file first.
- No `-p`. It would force the subcommand to become optional and add a conflict matrix against every subcommand. A subcommand adds none of that. Inline `-p "text"` can return later as sugar without breaking anything.
- Not `ahi run`: that is the hook runner. `ahi exec` is the name.

## Proposed CLI contract

- [ ] `ahi exec --prompt-file <path> --model <gateway/model-id>`. The file is read once as UTF-8, verbatim: no templating, no wrapping. Reject a missing file, a directory, empty input, invalid UTF-8, and oversized input (documented cap). `-` is a file name like any other, not stdin.
- [ ] `--model <gateway/model-id>` is required in v1. Resolve through the existing curated `models.json` and global directory flags: no picker, no TUI default, no mutable last-selection fallback. Actionable errors for a missing file, unknown gateway or model, and missing credential. Never trigger browser login, catalog refresh, or any interactive setup; a missing or invalid credential points at `ahi login`. Each worker machine holds its own credentials and curation; the coordinator never ships tokens. An expired credential may be refreshed non-interactively (existing behavior); a failed refresh ends the turn with the login guidance and a distinct exit code.
- [ ] `--thinking <off|low|medium|high>`, default off (omit reasoning entirely, the TUI's Off). A provider rejection of the level is a turn failure, not a silent downgrade.
- [ ] `--workdir <dir>` (default: the process's current directory) is the worker's working directory: the root for every builtin tool, for `run_command`'s start directory, and for policy rule paths. It must exist and be a directory. Containment is the existing rule: absolute paths, `..`, and symbolic links are refused, so a rule's `path` narrows access and never widens it.
- [ ] `--tools <policy-file>` is the worker's contract, and it can differ per delegated task. Headless runs never read the workdir's `ahi.tools.yaml` implicitly: without `--tools`, no tool is advertised and any requested call fails closed. With it, the file must exist and parse (as for `ahi tui --tools`), or the run exits before any model call.
  - Only `approve` rules without `ask` run. An `ask` rule has nobody to answer it, so it is treated as `deny`, with the reason `requires approval; none available headless` returned to the model as the tool result. Never auto-approve an `ask`.
  - A denied call is not a run failure: the model sees it and continues. The result file counts it.
  - Command rules run on the host with the operator's access and are not a sandbox. Run the worker inside `ahi dev run` for a container boundary, and say so in the docs. See [Tool calls](TOOL_CALLS.md).
- [ ] `--history-file <path>` carries continuity, owned by the caller. If it exists, its messages precede the prompt (a follow-up or steering turn); after a validated successful turn it is rewritten atomically with the new messages. On failure it is left unchanged, so retrying the same input is safe. Versioned JSON with a documented compatibility policy. Provider-native replay data is kept only when the protocol is unchanged, as in the TUI. If the file changed between read and write, fail with its own exit code instead of overwriting (no lock files). A schema this ahi cannot read is an error, never a silent restart.
- [ ] `--result-file <path>` receives one versioned JSON object, written atomically on every outcome:
  `{"schema": 1, "status": "ok"|"failed"|"cancelled"|"timeout", "gateway": "...", "model": "...", "text": "...", "error": {"kind": "...", "message": "..."}, "usage": {...}, "tools": {"calls": N, "denied": N}}`.
  `text` appears only for `ok`, `usage` only when the provider reported it. A coordinator reads one file and the exit code; stderr is diagnostics, never an interface. Without `--result-file`, `--format text` (default) prints the final answer on stdout and `--format json` prints the same object.
- [ ] Protect the operator-side files from the model. The prompt, history, result, and `--tools` files are not tools' business: when any of them sits inside `--workdir`, the builtin tools refuse it like `ahi.tools.yaml` and the state directory today, whatever the policy says.
- [ ] Limits: `--timeout <secs>` with a documented default for the whole run, and a bound on tool rounds (reuse the existing tool-loop limit, expose it as a flag only if hc needs it). A timeout is never retried.

## Output and exit codes

- [ ] stdout carries only the final answer (or the JSON object), buffered until a validated successful terminal event, written once. A failed, refused, truncated, or cancelled run leaves nothing on stdout that could be mistaken for an answer; its story is in the result file. Diagnostics go to stderr, sanitized: never secrets, prompt text, tool arguments, or upstream response bodies. UTF-8 out regardless of locale, no ANSI.
- [ ] Broken pipe: Rust ignores SIGPIPE, so a closed stdout surfaces as EPIPE. Exit quietly with the defined code; never panic, never treat it as a model failure.
- [ ] Exit codes, so a coordinator can react without parsing text (proposed; finalize with hc):

| Code | Meaning |
| --- | --- |
| 0 | Validated successful completion |
| 1 | Turn failure (transport, malformed stream, refusal, truncation) |
| 2 | Usage error (bad arguments, unreadable or invalid prompt file, bad `--workdir`) |
| 3 | Configuration or curation error (missing or invalid `models.json`, unknown model) |
| 4 | Credential missing or refresh failed; `ahi login` required |
| 5 | Policy file missing or invalid (fail closed before any model call) |
| 6 | Timeout or cancellation |
| 7 | History file unreadable, unsupported schema, or changed during the run |

- [ ] Timeout and cancellation: the backend is a worker thread plus channel, so implement the wall-clock limit as `recv_timeout` on the response receiver, then `CompletionBackend::cancel()` and drop the receiver (the documented path). On SIGINT/SIGTERM, cancel, write the result file with `cancelled`, and exit 6. A running `run_command` is killed with the run.

## Implementation

- [ ] `src/main.rs`: add an `Exec` subcommand; every existing subcommand, global flag, and help or error path stays unchanged. Proposed module `src/chat/exec.rs`, keeping terminal modules untouched.
- [ ] Headless turn loop: build the `ChatRequest` (selection, thinking, history plus the prompt as one user message), construct the same `HttpBackend` the TUI uses with `--workdir` as the tool root, drain the `Receiver<ResponseEvent>`, run the existing tool loop with an approver that answers `ask` as deny, accumulate `TextDelta`, capture `Usage` and context window if reported, and treat `Completed` as the only success. Map `Failed { kind, .. }` to exit codes. Preserve every boundary from [OpenAI inference](OPENAI_INFERENCE.md) and [Providers](PROVIDERS.md): credential destination checks, stream limits, terminal-event reconciliation, no automatic generation retry (429 and 5xx surface as failures with exit 1; rate-limit handling is the coordinator's job).
- [ ] No terminal initialization, no crossterm, no TUI event loop; stdin is never read, so the process works with stdin closed and stdout fully redirected. A `run_command` child gets no stdin either.
- [ ] Writes by tools go through the existing atomic writer and append their hash-only line to `audit.log`; confirm the audit line identifies the run as headless.
- [ ] Side effects of a normal run: credential rotation on refresh (atomic replacement), tool effects the policy approved, and the files the caller named. Honor the global `--state-dir`, `--cache-dir`, and `--config` flags so a coordinator can run workers hermetically with disposable state. No logs by default; any logging must never contain prompt or answer text.
- [ ] Concurrency: parallel `ahi exec` processes are safe (no shared mutable state, no lock files) except for two runs sharing one history file, which the changed-during-run check catches. Two workers on one `--workdir` are the coordinator's responsibility; document it.

## Verification and rollout

- [ ] Integration tests with disposable state and the local loopback mock gateway (the pattern from [OpenAI inference](OPENAI_INFERENCE.md)); no real credentials, no external network. Cover:
  - input: the prompt file; missing, empty, oversized, and invalid files; stdin closed or a TTY (ignored either way); missing options.
  - models and credentials: model resolution and curation errors, missing credential.
  - output: text and JSON success; result file on every status; stdout empty on failure; Unicode and multibyte fragmentation; EPIPE; stdout/stderr separation; no-terminal operation.
  - tools and workdir: no `--tools` means nothing advertised and calls fail; `ask` answered as deny with the reason; approved read, edit, and command rules; workdir escapes, `..`, and symlinks refused; `run_command` starts inside the workdir; operator files inside the workdir unreadable and unwritable by tools; a tool-only turn followed by an answer.
  - continuity: history round trip, unchanged on failure, changed-during-run exit 7, unsupported schema.
  - lifecycle: malformed or incomplete streams, refusal, truncation, timeout, cancellation.
- [ ] Regression: existing subcommands, TUI paths, and the clap help and version surface; the repository's normal format, lint, and test checks; Windows behavior (no SIGPIPE) alongside Unix.
- [ ] Rollout in slices, each shippable: (1) `exec` with `--prompt-file`, `--model`, text and JSON, no tools; (2) `--workdir` and `--tools` with `ask` as deny; (3) `--result-file` and exit-code taxonomy; (4) `--history-file`. Then agree on the JSON schemas, exit codes, and timeout semantics with hc and add an hc-side integration test that launches the real binary against the mock gateway. Document invocation, security, limits, and exit codes in [`README.md`](../README.md) (Commands table and a section) and record each slice in [`CHANGELOG.md`](../CHANGELOG.md).

## Later, not in this plan

- Delegation from a main ahi session: a builtin tool that asks the coordinator (hc) to start a worker and returns its result, so the main thread can fan out. ahi still does not start or schedule workers itself.
- Steering a running worker (beep-boop's `message_task`) needs a live channel, not files; design it separately.
- An events file or JSONL stream for progress. It conflicts with the buffered-until-success output guarantee and needs its own versioned contract.
- Inline `-p "text"` or stdin input as sugar that writes a temporary prompt file. Only if a human-facing need appears; the worker contract stays file-only.

## Open questions

- Does hc need the history file's contents to be inspectable, or only opaque and round-trippable? That decides how much of the format is documented as stable.
- Should the result file also be written when the run fails before the model call (usage errors, bad policy)? Proposed: yes whenever `--result-file` is writable, with `status: failed` and the error kind.
- Should `--tools` accept the shipped example names (`--tools read-all`) as well as paths? Proposed: paths only; `ahi tools init --example` already writes a file to start from.
