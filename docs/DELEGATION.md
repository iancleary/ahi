# Delegation from a chat session (`delegate`)

Status: design draft, not implemented. It depends on [Headless worker turns](HEADLESS_EXEC.md) (the worker side) and on hc's command surface, which is not yet confirmed. Everything named below is proposed syntax. Where this document assumes something about hc, the assumption is marked **(assumed)** and listed again at the end.

## The gap this closes

`ahi exec` lets a coordinator run one bounded turn on a worker host. This document is the other direction: the model in a main `ahi tui` session decides that part of its task belongs to a worker, hands it off, and gets the result back as a tool result. In beep-boop terms this is the main chat spawning a task and reading its reply. The roles are the ones [Headless worker turns](HEADLESS_EXEC.md#roles) sets out:

| Role | Who | In this document |
| --- | --- | --- |
| Coordinator (decides) | the model in `ahi tui` | calls `delegate` |
| Coordination plane (sees and carries) | hc | places files, picks the host, runs `ahi exec --headless`, keeps task state and history, shows the human |
| Worker (acts) | `ahi exec` on some host | unchanged |

ahi still never chooses a host, opens a session, or copies a file to another machine. `delegate` talks to hc, and only to hc.

```text
ahi tui (coordinator)                  hc (plane)                               worker host
  model calls delegate ───▶ writes task.md, runs
                            hc task run --worker <label> ──▶ places files, ssh, ahi exec --headless ──▶ turn
                            waits (bounded) ◀── result.json ◀───────────────────────────────────────
  tool result = text + effects ◀──────
```

## Why a builtin tool, and why hc on PATH

- **A tool, not a slash command.** The point is that the model fans out: it reads a result and decides the next step. A human-typed command would make the human the coordinator, which `ahi exec` from a shell already covers.
- **hc as a program with argv, not a library or a socket.** ahi has one protocol to hc then: a command line, a result file, an exit code. That is the same discipline the worker contract uses, it keeps hc's internals out of ahi, and it means a missing hc is a plain "program not found" at policy-load time rather than a linking or version problem. It matches the repository's posture of running tools that are already installed and never fetching one.
- **Governed by `ahi.tools.yaml`, bounded by labels.** A repository may say "this model may delegate to workers labeled `rust-ci` and `docs`". It may not say where those are. hc resolves labels to hosts from its own registry, which lives on the hc host and which no other machine reads.

## Proposed policy rule

A new rule kind in `ahi.tools.yaml`, alongside `tool_call`, `file_write`, and `command`:

```yaml
version: 1
rules:
  - id: delegate-ci
    action: delegate
    match: { worker: rust-ci }
    effect: approve
    ask: true
  - id: delegate-docs
    action: delegate
    match: { worker: docs }
    effect: approve
  - id: no-prod
    action: delegate
    match: { worker: prod-* }
    effect: deny
```

- [ ] `action: delegate` governs the `delegate` tool only. `match.worker` is required: a label, or a label prefix ending in `*`. No `match.path`, no `match.argv`. Deny beats approve, as everywhere in the file.
- [ ] `ask: true` prompts in the chat with the label, the prompt's line count, and nothing else, matching the write prompt's counts-only rule. `s` grants that label for the session, bound to the working directory, gateway, and destination like every other grant.
- [ ] The tool is advertised only when an approve rule names at least one label, and only when `hc` is on `PATH` at policy load. A policy that approves delegation on a host without hc is an error at load, not a silent no-op: the model would otherwise be told it has a tool that cannot work.
- [ ] `PLANNED_KINDS` already reserves `vcs_push`, `outbound_message`, and `pre_commit`; `delegate` joins it until implemented, so a rule written early fails with "not implemented yet" rather than "unknown action".

## Proposed tool

```text
delegate(worker: string, prompt: string, task?: string, timeout?: integer)
```

- `worker`: the label. Must match an approve rule; a label no rule names is denied before hc is consulted.
- `prompt`: the task text, written verbatim to the task file. Bounded by the worker's prompt cap (1 MiB).
- `task`: optional task id from an earlier `delegate` result, to send a follow-up to the same worker conversation. hc maps the id to the worker's `--history-file` and passes the same path again; ahi only passes the id back. A new id is minted by hc when omitted.
- `timeout`: seconds to wait for the result, default and maximum documented; the worker's own `--timeout` is hc's to set from this.

Result content returned to the model, bounded like every tool result:

```text
task <id> on <label>: <status>
<worker text, or the error kind and message>
effects: none | some | unknown
```

The result file hc returns is the worker's result object from [Headless worker turns](HEADLESS_EXEC.md#result); `text`, `status`, `error`, and `effects` are what the model sees, and `ledger` is shown on screen the way a `run_command` result shows its streams. The screen's `ToolDetail` gains a `Delegate` variant for that.

The result's `history` object (`status`, `session`, `tip`) is not shown to the model. It is kept so the main session's own `turn` line can point at the worker's session file:

```json
{"type":"turn","id":"…","parentId":"…","run_id":"…","status":"ok",
 "delegations":[{"task":"t-7f3a","worker":"rust-ci","run_id":"…","session":"<worker session id>","tip":"<entry id>"}]}
```

That is the whole cross-chat link: a main-session line names a worker session by id, and `ahi session show <id>` on the worker host (or hc running it over ssh) opens it. Nothing is copied into the main transcript.

### Prior art: the local Pi `delegate` extension

The Pi extension installed under `~/.pi/agent/extensions/delegate` already does the single-host version of this tool and is the shape to stay compatible with where it costs nothing: it takes `task`, `cwd`, and `timeoutSeconds`, and returns `status` as one of `completed | failed | cancelled | timed_out`, a `selection` (which model ran), `output`, and `usage`. The mapping to the worker result is direct:

| Pi extension | worker result |
| --- | --- |
| `completed` | `status: ok` (exit 0) |
| `failed` | `status: error` with `error.kind` from exit 1, 3, 4, 7 |
| `cancelled` | exit 5 (signal) |
| `timed_out` | exit 6 |
| `selection` | `gateway` and `model` |
| `output` | `text` |
| `usage` | `usage` |

The model-facing content above adds `effects`, which the Pi extension does not have and which is the one field a coordinator needs before deciding whether to re-read files.

## What hc has to offer **(assumed)**

The whole interface ahi needs from hc, as a subprocess:

| Call | Purpose |
| --- | --- |
| `hc task run --worker <label> --prompt-file <path> --result-file <path> [--task <id>] [--timeout <secs>]` | place the prompt on the worker host, run `ahi exec --headless …` there, copy the result back to `--result-file`, exit with the worker's exit code (255 for a lost session) |
| `hc task run … --check` or `hc worker check <label>` | preflight a label without running a turn, for the advertise-at-load decision |

ahi runs hc with the hook runner's scrubbed environment plus `PATH`, stdin null, output captured and never shown to the model (hc's stderr is the human's, in hc's own session). ahi writes the prompt to a private temporary file under its state directory, never under the working directory, and removes it after the call; hc copies it from there. ahi reads the result file hc wrote, validates `schema`, and deletes it. The task id and label go into the audit log as a `delegate` event with the worker's `run_id`; the prompt does not.

If hc's actual surface differs (a daemon, a socket, a different argv), only the executor behind the `delegate` tool changes. The policy rule, the tool schema, and what the model sees do not, which is the reason to fix those first.

## Boundaries that do not move

- The model cannot name a host, a path on another machine, a policy for the worker, or a model for the worker. hc's registry maps a label to all four. If a task needs a different policy, the operator adds a label.
- Delegation is not a sandbox and does not change what the local tools may do. A delegated task runs with whatever `task.tools.yaml` hc's registry assigns to that label on that host.
- `delegate` does not steer a running worker, stream its progress, or cancel it. One call, one turn, one result. Steering and liveness are hc features in hc's own view; a follow-up is a new `delegate` call with the `task` id.
- The main session's history records the `delegate` call and its result like any tool exchange and replays them, and its `turn` line carries the `delegations` list above. The worker's history is an ordinary `ahi` session file on the worker host, written by `ahi exec --history-file` at a path hc chooses; it is never pulled into the main transcript; `ahi session show` reads it in place and `ahi session import` on the hc host puts it in the queryable store.
- A failed delegation (hc missing, exit 255, result file absent or unreadable) is a tool error with `effects: unknown`, exactly what [Headless worker turns](HEADLESS_EXEC.md#output-progress-exit-codes-signals) tells a coordinator to assume. The model is told so and decides; ahi never retries a delegation on its own.

## Slices

1. Policy: `action: delegate` parsing, `match.worker` labels and prefixes, advertise-at-load with the hc presence check, `ask` and session grants for a label. Shippable alone: the tool exists in the schema and every call fails with "not implemented" until slice 2.
2. Executor: the hc subprocess call with a fixed argv, temporary prompt file, result validation, bounded wait, audit event, `ToolDetail::Delegate`. Tested with a stand-in `hc` script on `PATH` that writes a canned result file, so no real hc or ssh is needed.
3. Follow-ups: the `task` argument, the `delegations` list on the main session's `turn` line, and hc's history round trip, once `--history-file` has shipped on the worker side (headless exec slice 4a) and the chat appends its own turns (slice 4c).

An hc-side integration test, launching real `ahi` on both ends against the mock gateway, belongs to hc's repository and gates calling the interface stable.

## Assumptions to confirm with hc

1. hc is invoked as a program on `PATH` with an argv of the shape above, and exits with the worker's exit code.
2. hc places the prompt from a local path ahi gives it and returns the result to a local path ahi names, so ahi never reads or writes a remote path.
3. hc mints and owns task ids and maps each to a worker `--history-file` path; the file itself is ahi's session format, and the coordinator's ahi never reads it, only records the session id hc returns.
4. A label unknown to hc is a distinct, machine-readable failure, so ahi can report "no such worker" rather than a generic error.
5. hc's registry is local to the hc host; a coordinator's ahi never needs it, only `PATH` to hc.

## Open questions

- Should an `ask` prompt show the first line of the task text? The write prompt never shows content; the command prompt shows the full argv because it is the thing being approved. A delegated prompt is closer to content. Proposed: label and line count only.
- Should `delegate` be usable from `ahi exec` itself (a worker that delegates further)? Proposed: no in the first version; fan-out is the coordinator's, and nested delegation needs a depth limit and a way for hc to attribute cost.
- Does the result's `ledger` belong in the model-facing content or only on screen? Proposed: screen only; `effects` is what the model needs to decide whether to re-read files.
