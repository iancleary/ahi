# ahi as an addressable service (`ahi serve`, `ahi exec`, `ahi connect`)

Status: shipped in the sequence of PRs that replaced the earlier headless-exec plan. This is the design as built; `README.md` has the command surface.

## Why

A chat in a terminal can be watched but not addressed: nothing can type into it, follow its progress as data, answer an approval for it, or cancel it from elsewhere. A coordinator that wants to run bounded tasks on worker hosts needs exactly those four things, and herdr-core (hc), the terminal multiplexer that opens shells on worker hosts over `ssh` and `tailscale ssh`, already has the pattern for them: one core per host speaking JSON lines over a Unix socket, bridged across machines by `ssh host hc connect`. ahi adopts that pattern instead of inventing a parallel one.

## Shape

One `Service` object owns the session store and runs turns through the same engine the chat screen uses: `HttpBackend`, `ToolRuntime`, `Recorder`. The session is the resource. A turn is submitted to it; its entries and the streamed text are events a subscriber follows; an approval the policy could not decide is an `ask` event a client answers; a cancel is a request. One session has at most one open turn; every turn runs on its own thread.

Two front ends reach the service the same way:

| Program | How it reaches the service |
| --- | --- |
| `ahi serve` | wraps a `Service` in a listener on a Unix socket; one process per user per host; never idles out |
| `ahi exec` | a client: over the socket when a server answers, else a `Service` built in its own process, same calls |
| `ahi connect` | copies stdin and stdout to the socket, so `ssh host ahi connect` is a remote connection |
| `ahi tui` | records through the same `Recorder` today; becomes a client in the next PR |

There is no network listener, no token, and no TLS. The socket is mode 600 in a directory of mode 700 (`$XDG_RUNTIME_DIR/ahi/serve.sock`, else `<state>/run/serve.sock`); whoever can open it is the user who owns it. Across machines, ssh carries the bytes and authenticates, exactly as it does for hc's own cores. hc still decides which host does what; ahi never holds a host registry and never resolves a worker label.

## Wire protocol

One JSON object per line in each direction, over the socket or the stdio bridge. A request is `{"id": n, "method": "...", "params": {...}}` and gets one answer, `{"id": n, "result": ...}` or `{"id": n, "error": {"kind": "...", "message": "..."}}`. After `session.subscribe`, the session's events arrive on the same stream as objects with an `"event"` key and no `id`.

| Method | Params | Result |
| --- | --- | --- |
| `health` | | `version`, `store`, `open_turns` |
| `shutdown` | | `{}`; the server cancels open turns, waits up to 5 s, exits |
| `session.list` | | one object per session, newest first |
| `session.create` | `cwd`, `gateway`, `model`, `thinking?` | the header |
| `session.get` | `session` | `header`, `tip`, `active_parent`, `settings`, `open_run_id` |
| `session.entries` | `session`, `after?` or `branch?` | entries after `after` in write order, or the branch ending at `branch` |
| `session.subscribe` | `session`, `after?` | `{}`; entries after `after` arrive first, then everything live |
| `session.settings` | `session`, `gateway`+`model?`, `thinking?` | the settings; refused while a turn runs |
| `turn.submit` | `session`, `prompt`, `workdir`, `run_id?`, `prompt_sha256?`, `tools?`, `timeout?`, `thinking?`, `unattended?`, `origin?`, `client?` | `{"run_id", "status": "running"}`; the stored result document when `run_id` repeats with the same prompt; `conflict` when a turn is open |
| `turn.get` | `session`, `run_id` | the result document, or `status: running` |
| `turn.cancel` | `session`, `run_id` | `{}`; the turn closes as `cancelled` |
| `ask.answer` | `session`, `ask_id`, `allow: [bool]` | `{}` |

Events: `entry` (a line written to the session, with its `seq`), `text` (streamed assistant text not yet an entry), `ask` (`ask_id`, the calls with tool, target, and detail), `turn` (`status` and the result document), `keepalive` every 15 s on an idle subscription. A subscriber that falls 256 events behind is dropped and resumes with `after` set to the last `seq` it saw; entries are the durable record and `text` is repeatable only from the entry it ends up in.

Error kinds: `usage`, `config`, `auth`, `policy`, `session`, `conflict`, `not_found`; a closed turn's document carries `turn`, `transport`, `protocol`, `limit`, `auth`, or `policy` as its `error.kind`.

## Turn semantics

- `workdir` is canonicalized and must be a directory; the engine confines every tool to it as `ahi tui` does. `tools` is an explicit policy file, validated before the turn and refused inside the workdir.
- The model and credential are resolved before the turn so a bad model or missing credential is a refusal (`config`, `auth`) and nothing is recorded for it.
- `run_id` is idempotent per session: a repeat with the same prompt (checked by SHA-256) returns the stored document without a model call; a repeat with another prompt is `conflict`. `prompt_sha256`, when given, must match the prompt or the submission is refused.
- `timeout` bounds the turn: no tool call starts past it, a command's own timeout is clamped to what remains, the request's own timeout is shortened to match, and at the deadline the turn is cancelled and closed as `timeout`. A cancelled or timed-out turn gives the engine 3 s to report and then closes; its `effects` is `unknown` when a call was in flight.
- An `ask` becomes an `ask` event when a subscriber is attached and `unattended` is false; otherwise every call in the batch is answered `Unattended`, a denial whose reason tells the model no operator is attending.
- The result document: `schema`, `ahi_version`, `run_id`, `session`, `status` (`ok`, `failed`, `cancelled`, `timeout`), `origin`, `client`, `gateway`, `model`, `text` and `text_truncated` for `ok`, `error` for `failed`, `usage` when reported, `tools` (`calls`, `denied`, `ask_denied`, `errors`, `rounds`), `effects` (`none`, `some`, `unknown`), the `ledger` (one record per effectful call, the same facts as `audit.log`), and `prompt_sha256`. The `turn` entry in the session holds the document without `text`.

## `ahi exec`

The command a person types and the worker a coordinator runs over ssh. The prompt comes from `-p`, `--prompt-file`, or stdin; `--headless` takes it only from a file, answers every `ask` as denied, and requires `--result-file`. Operator files (`--prompt-file`, `--result-file`, `--tools`) must lie outside `--workdir`, where the model's tools cannot read or overwrite them. The result file must not exist beforehand and is written owner-only through a rename. stdout carries the answer (or the whole document with `--format json`), stderr the progress lines (`run <id> tool <name> <target> exit <code>`; never arguments or output), and the exit code says how it ended: 0 ok, 1 turn failed, 2 usage, 3 configuration, 4 credential, 5 policy file, 6 cancelled or timed out, 7 session, 8 result file not written. SIGINT, SIGTERM, and SIGHUP cancel the turn, which closes as `cancelled` with its ledger. `--check` validates everything and makes no turn. `--session <id>` continues a recorded session; `--socket` insists on a server; `--standalone` insists on the in-process path. `ahi exec` never starts a server.

## Later

- `ahi tui` as a client: start the server when none is live, attach locally or over `--attach "ssh host ahi connect"`, render from events, answer asks, cancel. Then the chat and an hc space can share a live session.
- Delegation from a chat session over the socket (`docs/DELEGATION.md` to be rewritten against this protocol).
- Import and export over the wire; a Windows named-pipe listener; per-tool OSC 7501 child records.
