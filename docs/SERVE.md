# ahi as an addressable service (`ahi serve`)

Status: design plan, not implemented. Builds on `docs/HEADLESS_EXEC.md` (slices 1 to 4d) and changes one of its decisions on purpose: ahi gains a long-running process per host that owns the session store and answers HTTP. Everything else in that plan stands and is what this one is built from.

## Why

With `ahi exec`, the only path between a coordinator and a worker is a shell hc holds over ssh: files in, files out, one exit code, `kill` to cancel. That is enough to run a bounded task, and it stays the fallback. It is not enough for an hc space to be an interactive surface over a live session:

- Nothing can type into a running `ahi tui`; a space can only watch its pty.
- A session can be continued by `ahi exec --session <uuid>` between turns, but a tui and an exec on the same session contend for Turso's process lock and one of them exits 7.
- Progress is a `tail -f` on a JSONL file over ssh; there is no cancel, no approval relay, and no event stream.

The decision taken here: the session becomes a resource served by one `ahi serve` process per host, and every other entry point (`ahi tui`, `ahi exec`, `ahi session`) becomes a client of it when it is running. One process owns `sessions.db`, so the lock stops being a problem; a human and a space can both send turns to one session and both see the same stream; cancel and approval are requests, not signals.

What this does not change: hc still decides which host does what. ahi never learns about hosts, never holds a registry, and never resolves a worker label. hc starts `ahi serve` on a host (or a unit file does) and then talks to it. `ahi.tools.yaml` still answers "what may the model do here" and still never names a machine.

## Process model

One `ahi serve` per user per host, bound to a Unix socket by default.

```text
hc space / ahi tui --attach / ahi exec (client)          worker host
   POST /v1/sessions                       ─────────▶    ahi serve
   POST /v1/sessions/{id}/turns {prompt, run_id, …} ──▶      owns <state>/sessions.db
   GET  /v1/sessions/{id}/events  ◀── SSE ────────────      one thread per open turn
   DELETE /v1/sessions/{id}/turns/{run_id}  ──────────▶      same tool loop, same policy, same ledger
```

- The server is `ahi serve`, a subcommand like the others. It is the only process on the host that opens `sessions.db` while it runs.
- A turn runs on its own thread, through the same `ToolRuntime`, `HttpBackend`, and `Recorder` the tui and exec use today. Several sessions may have turns open at once; one session has at most one open turn (a second submission is `409 Conflict`).
- Clients discover the server through its socket path. When the socket exists and answers `/v1/health`, `ahi tui`, `ahi exec`, and `ahi session` route through it. `--standalone` forces the direct path and is refused when the socket is live, because two writers to `sessions.db` is the thing this plan removes.
- `ahi tui` starts the server when none is live. It spawns `ahi serve` detached from its own process group and terminal (stdio to `<state>/serve.log`), waits for `/v1/health` (up to 2 s), then attaches. The server outlives the tui: closing the chat leaves the socket up for the next tui, for `ahi exec`, and for hc spaces, which is the point of having one. It idles at no cost beyond the open store, and `ahi serve stop` ends it (a `POST /v1/shutdown` over the socket only, never over TCP). Two tuis started at once race on the bind: the loser sees `EADDRINUSE`, waits for health, and attaches to the winner.
- `ahi exec` and `ahi session` never start a server. hc drives exec over ssh on hosts it has not set up a server on, and a one-shot command leaving a daemon behind would surprise it; they use the socket when it is live and the direct path otherwise. A server started by a tui is a server hc may also use once it knows the host has one.
- `ahi serve` has no model credentials of its own. It reads `models.json` and the credential file the way the tui does; a client never ships a token for the provider.

## Transport and authentication

Two listeners, chosen in argv, never inferred.

| Listener | Flag | Auth | Default |
| --- | --- | --- | --- |
| Unix socket `<runtime>/ahi/serve.sock` (mode 600, directory 700) | none needed | filesystem permission; no token checked | on |
| TCP | `--listen <ip>:<port>` | `Authorization: Bearer <token>` on every request | off |

- The token is 32 random bytes, hex, in `<state>/serve.token` (mode 600), created on the first `--listen` start and printed once with `ahi serve token`. Clients read the same file on the same host, or are handed it by hc, which already carries secrets to spaces. Comparison is constant-time.
- No TLS of ahi's own. The expected network is a tailnet, which encrypts and authenticates the peer; `--listen` refuses `0.0.0.0` and `::` unless `--listen-any` is also given, and the start-up line names the address it bound.
- No CORS headers, no cookies, no browser story. A request with an `Origin` header is refused. The clients are hc, `ahi`, and `curl`.
- `<runtime>` is `$XDG_RUNTIME_DIR` when set, else `<state>/run`. Windows gets TCP on loopback (`127.0.0.1:0`, port written to `<state>/serve.port`) because there is no Unix socket; the token is required there.
- Request bodies are capped (1 MiB prompt, 64 KiB otherwise); prompts and tool outputs are never logged by the server; the audit log gets `origin: "serve"` and the client-supplied `client` string, so a space can be told apart from a tui.

## API

Version prefix `/v1`. JSON bodies, `Content-Type: application/json`. Errors are `{"error": {"kind": "...", "message": "..."}}` with the `kind` values the result file already uses (`usage`, `config`, `policy`, `history`, `timeout`, `cancelled`, `turn`) plus `conflict` and `not_found`.

| Method and path | Does | Returns |
| --- | --- | --- |
| `GET /v1/health` | liveness, version, store location | `200 {"version", "store", "sessions_open"}` |
| `GET /v1/sessions` | what `ahi session list --json` prints | one object per session |
| `POST /v1/sessions` | create a session: `{cwd, gateway?, model?, thinking?}` | `201` header |
| `GET /v1/sessions/{id}` | header, tip, counts | `200` |
| `GET /v1/sessions/{id}/entries?after=<entry>&branch=<entry>` | the active path, or from `after` | entries in order |
| `POST /v1/sessions/{id}/turns` | submit `{prompt, run_id?, prompt_sha256?, workdir, tools?, timeout?, thinking?, client?}` | `202 {"run_id"}` running; `200` result when `run_id` is a replay; `409` when a turn is open |
| `GET /v1/sessions/{id}/turns/{run_id}` | the turn's result document, the same shape as the exec result file | `200`, or `202` while running |
| `DELETE /v1/sessions/{id}/turns/{run_id}` | cancel: sets the cancel flag the tool loop already honours | `202`; the turn closes with `status: cancelled` |
| `GET /v1/sessions/{id}/events?after=<entry>` | server-sent events for the session, live | stream |
| `GET /v1/sessions/{id}/asks` | approvals waiting on a human | list |
| `POST /v1/sessions/{id}/asks/{ask_id}` | `{"decision": "allow" \| "deny", "remember"?: bool}` | `204` |
| `POST /v1/sessions/{id}/settings` | `/model` and `/thinking` for a session: `{gateway?, model?, thinking?}`, recorded as `model_change` / `thinking_change` | `200` |
| `POST /v1/sessions/{id}/export` | the session as JSONL in the response body | `200 application/x-ndjson` |
| `POST /v1/sessions/import` | a JSONL body, same rules as `ahi session import` | `201 {"added"}` |

Turn semantics are the exec semantics, unchanged:

- `workdir` is resolved and checked as `--workdir` is; `tools` is a policy file path outside it, or absent to use the directory's `ahi.tools.yaml`; the prompt arrives in the body, so the "operator files outside the working directory" rule has nothing left to protect on the input side and still protects the result on the output side (results live in the store, not in a file the model can reach).
- `run_id` is idempotent per session with the same `prompt_sha256` rules as `ahi exec --run-id`.
- `timeout` is enforced inside the tool loop as today; the server adds nothing and removes nothing.
- An `ask` decision from the policy becomes an `ask` event with an `ask_id`. If a client answers within the turn's deadline, the answer is used; if no client is attached (the server knows, because events subscriptions are counted per session), the call is denied per call, exactly the `Unattended` behaviour exec has now. A submission may also say `"unattended": true` to skip the wait.
- The result document carries the ledger and `effects` as before, plus `client`.

### Events

`text/event-stream`, one event per line of the session as it is written, plus the streamed text that is not yet an entry:

```text
id: 9a1f3c2e
event: entry
data: {"type":"message","id":"9a1f3c2e","parentId":"...","message":{...}}

event: text
data: {"run_id":"...","delta":"the next few tokens"}

event: ask
data: {"ask_id":"...","run_id":"...","call":{"name":"run_command","argv":["cargo","test"]},"rule":"ask"}

event: turn
data: {"run_id":"...","status":"ok","exit_code":0}
```

- `id:` is the entry id, so `Last-Event-ID` or `?after=` resumes a dropped stream from the store with nothing lost: entries are the durable record and `text` deltas are repeatable only from the entry they end up in.
- The recorder gains one fan-out: every entry it appends is also offered to the session's subscribers over a bounded channel. A slow subscriber is dropped and must resume with `after`; the store never waits on a client.
- A heartbeat comment (`: keepalive`) every 15 s keeps proxies and tailscale idle timers quiet.

## Clients

- **`ahi tui`** attaches to the local socket, starting the server first when needed (above). `--attach <url>` attaches to a remote server over TCP with the token from `<state>/serve.token` or `AHI_SERVE_TOKEN`; `--standalone` is the old in-process path. In every mode it renders a session from `/entries` and `/events`, sends turns with `POST /turns`, and answers `ask` events in the approval dialog it already has. Without an argument it attaches to the local socket. `/model` and `/thinking` become `POST /settings`. Two tuis, or a tui and an hc space, on the same session see the same stream and take turns submitting; the `409` is shown as "a turn is running". The tui's local code path (today's `interact`) stays for hosts with no server.
- **`ahi exec`** on a host with a live socket submits its turn to the server, follows `/events` for the progress lines it prints today, and writes the result file from `GET /turns/{run_id}` so hc's file contract is unchanged. Exit codes map one to one from the result's `status`. `--headless` keeps meaning "no human, deny asks" by sending `unattended: true`. Without a socket, exec runs as it does now.
- **`ahi session`** commands call the server when it is live and open the store directly otherwise; output is identical either way.
- **hc** holds, per space, a base URL (or a socket and a host it reaches over ssh) and a session id. It creates sessions, submits turns with its own `run_id`s, renders `/events`, surfaces `ask` events to the human and posts the decision, and cancels with `DELETE`. ssh is still how hc starts `ahi serve` where no unit file does, and still how it reaches a host that only has `ahi exec`.

## Server lifecycle

- `ahi serve [--listen <ip>:<port>] [--listen-any] [--socket <path>]`. Start-up prints the socket path or bound address and the store location to stderr, then one line per turn opened and closed; `-v` adds requests.
- `SIGTERM`/`SIGINT` (through `ahi-signal`): stop accepting, set the cancel flag on every open turn, wait up to the grace already used by exec (5 s), close each turn with `status: cancelled`, exit 0. A second signal exits at once with 6.
- A stale socket file from a crash is detected by a failed `/v1/health` and removed before binding.
- Exit codes: 0 clean stop, 2 usage, 3 configuration (bad bind, token file unwritable), 6 forced stop, 7 store could not be opened (another process holds it).

## Dependency decision: `tiny_http`

The server needs HTTP/1.1 parsing, keep-alive, chunked responses for the event stream, and a Unix-socket listener. Options weighed:

| Option | Tree | `unsafe` | Fits |
| --- | --- | --- | --- |
| hand-rolled on `std::net` | 0 crates | 0 | owning request parsing, chunked framing, keep-alive and connection limits; roughly the size of a crate, with none of its test corpus |
| `tiny_http` 0.12 | 5 crates (`ascii`, `chunked_transfer`, `httpdate`, `log`, itself), no TLS, no async | 1 site | blocking, thread per connection, `Server::http_unix`, `Response` over any `Read` with `data_length: None` streams chunked |
| `hyper`/`axum` | tokio and 60+ crates | many | the async runtime the Turso decision just avoided |

Decision: `tiny_http`, used directly from `ahi` in a `serve::http` module. It adds one registry crate and four small transitive ones, the same bar `turso` met; it needs no wrapper crate because there is no FFI and `ahi` already depends on crates that contain `unsafe` (`ureq`, `regex`). Exact pin, bump with a CHANGELOG entry. If it is abandoned upstream, the hand-rolled option replaces it behind the same module boundary.

The client side is `ureq`, already present. Server-sent events on the client are a line reader over the response body; no new crate.

## Implementation slices

Each is a draft PR stacked on the previous; the first lands behind nothing because the socket is opt-in by the act of running `ahi serve`.

- **S1 serve over the socket.** `ahi serve` with `/health`, sessions, turns, turn results, settings; one thread per turn through the existing engine; `ahi exec` and `ahi session` route through the socket when live and refuse `--standalone` against a live socket. Test: a turn submitted over the socket against the fixture gateway produces the same result document and store rows as `ahi exec` does directly.
- **S2 events.** Recorder fan-out, `/events` with `after`, `text` deltas, keepalive; `ahi exec` follows it for progress. Test: a dropped stream resumed with `after` sees every entry exactly once.
- **S3 cancel and asks.** `DELETE` turns, `ask` events and answers, unattended fallback, drain on signal. Test: a cancelled `run_command` child is killed and the turn closes `cancelled` with `effects` reported; an unanswered ask is denied per call after the turn's deadline.
- **S4 TCP and token.** `--listen`, token file, bearer check, `--listen-any` guard, Windows loopback. Test: no token is 401; wrong token is 401 in constant time; `0.0.0.0` without the flag is exit 3.
- **S5 `ahi tui` as a client.** Start the server when none is live (detached spawn, health wait, bind race), attach locally or with `--attach <url>`, render from events, submit, answer asks, settings; `ahi serve stop`. Test: two tuis started together end up on one server; two attached clients see the same stream and the second submission during a turn is shown as running; closing the tui leaves the socket answering.
- **S6 import/export over HTTP and hc notes.** The two endpoints and a short section in `docs/DELEGATION.md` replacing "hc as a program on PATH" with "hc over the API" for hosts that run a server.

## Verification and rollout

- Integration tests drive `ahi serve` as a child over a socket in the fixture directory, with the fixture gateway from `tests/exec.rs`; no network listener in tests except S4's loopback cases.
- `cargo deny` and the published-member scripts gain nothing new (no workspace crate is added).
- Rollout: `ahi serve` is inert until started; hc adopts it host by host and keeps `ahi exec` over ssh for the rest.

## Later, not in this plan

- Delegation from a main session over the API (`delegate` builtin posting to a server hc names by label).
- Multiple users on one host (one server per user is the model; a shared server would need per-client identity in the token).
- A browser client. Everything above refuses `Origin`; lifting that means CSRF and CORS work this plan declines.
- Push notifications from server to hc (webhooks). `/events` is pull; hc keeps a stream open per space.

## Open questions

- Should a server a tui started stop itself after some idle period, or only on `ahi serve stop`? Proposed: never on its own; an idle server is an open file, and a space that returns after an hour expects it there.
- Does hc want one socket per host it reaches over ssh (`ssh -L` to the Unix socket) or TCP on the tailnet? Both work; the token story differs. Proposed: support both, document the ssh forward as the zero-config path.
- Should `ask` answers be allowed to say `remember` and write a rule? Proposed: no; approvals stay per call, as in the tui today, until the policy file gains a documented write path.
- Where does `<runtime>` go on macOS without `XDG_RUNTIME_DIR`? Proposed: `<state>/run`, created 700.

## Decisions taken in this revision

1. ahi becomes addressable: one `ahi serve` per user per host owns the store and answers HTTP; this reverses "ahi does not become a daemon" in `docs/HEADLESS_EXEC.md` and keeps everything else there.
2. The session is the resource. Turns, events, asks, cancel, and settings hang off `/v1/sessions/{id}`; there is no "chat" or "tui" resource.
3. Unix socket by default with filesystem permissions; TCP is explicit, bearer-token authenticated, and never binds every interface without a second flag. No TLS of ahi's own; the tailnet provides it.
4. Every existing entry point becomes a client when a server is live. `ahi tui` starts the server when none is and the server outlives it; `ahi exec` and `ahi session` never start one and keep their direct path when none is live. The result-file contract hc already uses is preserved by `ahi exec` as a client.
5. `tiny_http` is the server dependency, pinned, used directly; the client side stays on `ureq`.
