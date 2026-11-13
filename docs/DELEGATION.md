# Delegation from a chat session (`delegate`)

Status: shipped. The model in a chat (or an `ahi exec` turn) hands part of its task to a worker and reads the answer as a tool result. The worker is another `ahi serve`; the coordinator reaches it over the service protocol described in [SERVE.md](SERVE.md), through a connect command that herdr-core (hc) provides.

## Roles

| Role | Who | Owns |
| --- | --- | --- |
| Coordinator (decides) | the model in `ahi tui` or `ahi exec` | what to delegate, to which worker label, and what to do with the result |
| Coordination plane (sees and carries) | hc | the registry of labels and hosts, the ssh connections, the human's view |
| Worker (acts) | `ahi serve` on some host, started by hc inside a checkout | one turn per task, in the directory its server runs in, under that host's `ahi.tools.yaml` |

ahi never learns a host. A repository's `ahi.tools.yaml` names worker *labels*; the connect command resolves a label to a connection; the worker's own policy governs what the task may do there.

## The rule

```yaml
rules:
  - id: ci
    action: delegate
    match: {worker: rust-ci}
    effect: approve
    ask: true
  - id: no-prod
    action: delegate
    match: {worker: prod}
    effect: deny
```

`action: delegate` governs the `delegate` tool only. `match.worker` is required and is a label (the rule-id alphabet); `match.path`, `match.argv`, and `match.tool` do not apply. Deny beats approve, as everywhere in the file. `ask: true` prompts in the chat with the label and the task's line count, never its text; `s` grants that label for the session. The tool is advertised only when an approve rule names a label. `examples/tools/delegate.yaml` is a starting point.

## The tool

```text
delegate(worker: string, prompt: string, task?: string, timeout?: integer)
```

- `worker`: the label. A label no rule approves is denied before anything is contacted.
- `prompt`: the task, verbatim, at most 1 MiB.
- `task`: the worker session id from an earlier result, to continue that conversation on the worker.
- `timeout`: seconds to wait, default 600, at most 3600.

The model receives:

```text
task <worker session id> on <label>: ok | failed | cancelled | timeout
<the worker's answer, or the error kind and message>
effects: none | some | unknown
```

`effects` is the worker's own word on whether it changed its working directory, which is what a coordinator needs before deciding whether to look again. The coordinator's ledger and `audit.log` record the label, the worker session and run ids, the status, the effects, and the duration; never the task text or the answer.

## How the worker is reached

`AHI_DELEGATE_VIA` is an argv template with `{worker}` for the label; the default is `hc connect --worker {worker}`. ahi runs it with the hook runner's scrubbed environment plus `PATH`, and speaks the service protocol, one JSON object per line, over its stdin and stdout, exactly as `ahi tui --via` and `ahi exec --via` do and as hc reaches its own cores with `--via`. Whatever the command connects to must be an `ahi serve`: hc's job is to map the label to a host it has a connection to and run `ahi connect` there.

On the worker, `session.create` with no `cwd` opens the session in the directory the server runs in, and with no model uses the host's saved selection (`<state>/selection`, written by `ahi tui`). hc therefore starts the worker's server inside the checkout with `hc run --on box ahi serve`, after one `ahi tui` (or a written `selection` file) has chosen the model there, and places a policy with `hc file`. The task runs unattended: an `ask` on the worker is denied with a reason the worker's model sees.

## Assumption to confirm with hc

One: that hc offers a command of the shape `hc connect --worker <label>` whose stdio carries bytes to `ahi connect` on the host the label maps to, and exits with that command. Nothing else about hc is assumed. If the shape differs, `AHI_DELEGATE_VIA` carries the real one and nothing in ahi changes.

## Boundaries that do not move

- The model cannot name a host, a path on another machine, a model for the worker, or a policy for it. The label is all it has; the worker's host, server, and policy are hc's and the operator's.
- Delegation is not a sandbox. A task runs on the worker with whatever that host's policy approves.
- One call, one turn, one result. A follow-up is a new call with the `task` id. Steering and liveness are hc's, through the worker's socket and its OSC 7501 status.
- A failed delegation (the connect command missing, the worker refusing, the connection dropping) is a tool error with `effects: unknown`. The model is told so and decides; ahi never retries a delegation on its own.
- A worker does not delegate further in this version: fan-out is the coordinator's.
