# Audit of 0.16.0 service paths and the 0.17.0 plan

Scope: `src/serve.rs`, `src/service/{mod,wire,client,ledger}.rs`, `src/exec.rs`, `src/chat/backend/{service.rs,tools/delegate.rs}`, `src/session/{mod,store}.rs`, on `docs/0.17-plan`. Read-only. Line numbers are from that branch.

## Part A: findings, by severity

### High

**A1. A turn that hits an `ask` after its last subscriber left hangs the session.** `service/mod.rs:796` tests `live.subscribers.is_empty()`, but `subscribers` only loses a dead sender when `emit` (`:620-623`) fails a `try_send`. A client that disconnects mid-turn (ssh drop, `ahi exec` killed, `ahi connect` closed) leaves its sender in the vector until the next event; the ask is then inserted into `live.asks` (`:815`), the emit prunes the dead sender, and nobody can answer. The tool loop blocks on `decisions.recv()` with no timeout (`tool_loop.rs:322`). For `ahi exec` the turn ends at its `--timeout`; for the chat `timeout: None` (`chat/backend/service.rs:163`) so the turn is open forever, `live.open` stays `Some`, and every later `turn.submit` and `session.settings` on that session is `conflict` until the server restarts. Fix: in `handle`, emit first and then check `subscribers.is_empty()`; answer `Unattended` when it is. Also, when the last subscriber is pruned while `asks` is non-empty, drain `asks` with `Unattended`. Add a server-side ceiling on how long an ask may wait (e.g. 10 min) regardless of client.

**A2. A panic in a turn thread leaves the session permanently "running".** `Turn::run` (`:650-767`) has no drop guard. Any panic between `live.open = Some(...)` (`:476`) and `live.open = None` (`:760`), including `expect` at `:598` and `:718` or a bug inside `HttpBackend`, unwinds the thread, never writes the `turn` entry, never emits `Event::Turn`. Consequences: `exec::follow` (`exec.rs:376`) loops forever on keepalives (it has no client-side deadline), `delegate::run` returns "did not answer in time" only after `timeout + 5 s`, the chat waits forever, and the session is `conflict` until restart. Fix: a guard struct in `run` whose `Drop` writes a `turn` entry with `status: failed, error.kind: turn, message: "the turn thread panicked"`, clears `open` and `asks`, and emits `Event::Turn`. Separately give `exec::follow` its own deadline of `timeout + GRACE + CANCEL_GRACE` and exit 6 with a message naming the run.

**A3. `ensure_running` race produces two servers and an orphan.** `serve::run` removes any existing socket file before binding (`serve.rs:105-109`) after a single `try_connect` probe (`:94`). Two chats starting at once: A passes the probe and binds; B passed the probe before A bound, unlinks A's socket, binds its own. A now listens on an unlinked inode forever (the server "never idles out"), holding `Live` state for sessions B will also open from disk, and `ahi serve stop` can only reach B. The docstring at `:159` ("the loser finds the winner answering") is not what the code does. Fix: take an exclusive `flock` on `serve.lock` in the socket directory for the whole life of the server (one `extern` in `ahi-unix`, same as the planned `SO_PEERCRED`); a second `run` that cannot take the lock exits with "a server already runs". Only remove the socket when the lock is held. This also gives `stop` a pid without a `serve.pid` file.

**A4. Appending after a torn tail corrupts the session for good.** `Session::read` (`store.rs:174-177`) skips an unterminated last line, but keeps `path` and later `write_lines` (`:268-280`) appends with `O_APPEND` after the torn bytes, producing one merged, unparsable line. The next `open` returns `Corrupt` and the session is unreadable by every reader, forever. A server crash mid-write (the exact case the torn-line rule exists for) followed by one `--resume` or one `exec --session` is the trigger. Fix: `read` records the byte offset of the last complete line; the first `write_lines` truncates to that offset (`set_len`) before appending, or `read` truncates at once when the file is opened for writing.

**A5. The client's line reader is unbounded; a hostile worker can exhaust the coordinator.** `client.rs:204` uses `BufReader::lines()` with no cap. Over `--via` and `delegate` the peer is another host; a compromised or misbehaving worker sends one line of unbounded length and the coordinator's reader thread allocates until OOM. The server side caps at 2 MiB (`serve.rs:83`); the client has nothing. Same fix as the server: `take(MAX_LINE+1).read_until(b'\n')`, drop the connection on overflow and surface it as `Disconnected`. Related: `delegate.rs:169-173` appends `error.message` from the worker's document uncapped into the model's tool result; cap it like `text`.

### Medium

**A6. Two servers can write one session file.** `Live` is never evicted (`service/mod.rs:572-581`), and the in-memory `Session` assumes it is the only writer. `ahi session import` of the same id, a standalone `ahi tui --resume` on the host, or the orphan in A3 each append to the file the server also appends to; the server's next write produces a duplicate `seq`, and `read` refuses the file as out of order (`store.rs:187`). Fix: hold a `flock` on the session file while `Live` holds it; `import`, standalone resume and a second server fail fast with `conflict`. Cheaper interim: before each `write_lines`, compare the file length to the length last written and refuse with `Corrupt` if it moved.

**A7. Silent subscriber drop hangs the client.** When a subscriber falls 256 behind, `emit` drops its sender; the forward thread sees `Disconnected` and returns (`serve.rs:382`) without telling the client anything. The client's `events` receiver is tied to the socket reader (`client.rs:202`), not to the subscription, so `next_event` keeps timing out and `exec::follow` spins forever. Fix: forward sends `{"event":"lagged","session":..}` before returning; clients treat it as "resubscribe from the last seq seen" or fail.

**A8. `timeout` from the wire can panic the connection thread.** `wire.rs:145` accepts any `u64`; `service/mod.rs:470` does `Instant::now() + timeout`, which panics on overflow (`"timeout": 18446744073709551615`). The panic is on the `serve_connection` thread, which dies without setting `closed`, so its forward threads linger until a write fails. Clamp `timeout` to a maximum (3600 s, as `delegate` already does) and refuse larger with `usage`. Also validate `run_id` server-side the way `exec.rs:147-156` does on the client; it names a thread and an ask id and is written unvalidated into the record. Bound `client` (`wire.rs:149`) to 64 bytes.

**A9. `Client::via` scrubs `SSH_AUTH_SOCK`.** `BASE_ENV` (`local_hook.rs:202-219`) has `HOME` and `PATH` but no `SSH_AUTH_SOCK`, so `ahi tui --via "ssh host ahi connect"` and the default `hc connect` chain run without an agent and fail key authentication unless an unencrypted key sits in `~/.ssh`. The scrub is right for hooks; for a transport it removes the one variable the transport needs. Fix: a separate allow-list for `via` (`BASE_ENV` + `SSH_AUTH_SOCK`, `SSH_AGENT_PID`), documented in `SERVE.md`.

**A10. `--via` is whitespace-split in three places, not one.** H3 names `AHI_DELEGATE_VIA` (`delegate.rs:62`), but `exec.rs:242` and `tui/mod.rs:121` split the same way. All three must go through `shlex`.

**A11. Delegation stderr and the chat's terminal.** `Client::via` inherits stderr (`client.rs:101`). In the chat, `hc` or `ssh` diagnostics are written straight into a raw-mode screen. Capture stderr on a thread, cap it (8 KiB), and show it in the tool result on failure.

**A12. UI thread can block up to 30 s on a dead server.** `Remote::call` waits 30 s for an answer (`client.rs:248`) while holding the `Arc<Mutex<Client>>`. `ServiceBackend::cancel` runs on the screen thread (`service.rs:208-212`); if the server is wedged, the screen freezes for 30 s. Make cancel fire-and-forget on a thread, and shorten the call timeout for `turn.cancel` and `ask.answer`.

**A13. Cancel of a delegation waits the full worker timeout.** `delegate.rs:123-129` sends `turn.cancel` once, then keeps waiting for the `turn` event until `deadline` (up to 3605 s). If the worker ignores the cancel or is partitioned, the coordinator's tool call cannot return. After the cancel, wait at most `CANCEL_GRACE + 2 s`, then return `effects: unknown`.

### Low

**A14. Socket mode race and pre-existing directory mode.** `set_permissions(0o600)` runs after `bind` (`serve.rs:110-115`); a socket under `--socket` in a shared directory is briefly umask-mode. `create_private_dir` returns `Ok` for a pre-existing directory of any mode (`directories.rs:81`). Bind to a temporary name in a private directory and `rename`, and run `exposed()` on the parent, refusing a group- or world-accessible one.

**A15. `connect` exits 0 whichever side closed** (`serve.rs:254-270`), so a coordinator cannot tell "stdin EOF" from "server died mid-turn". Record which side ended; exit 0 on stdin EOF, 3 on server close. H1's exit-code row needs this distinction.

**A16. `inside()` returns `false` when the parent does not exist** (`exec.rs:521-524`). `--result-file <workdir>/new/out.json` passes the check; if the model creates `new/` during the turn, the result is written inside the workdir. Resolve the nearest existing ancestor instead of giving up.

**A17. Worker-returned `session` and `status` are not validated** (`delegate.rs:87-92, 156`) before entering the ledger, the audit line and the model's context. Apply `valid_id` to `session` and restrict `status` to the four known values.

**A18. `shutdown` keeps accepting during the grace window.** The accept thread is never stopped (`serve.rs:153`); a `turn.submit` during the 5 s drain starts a turn that dies with the process, unrecorded. Set a refusing flag on `stop` and return `conflict: shutting down`.

**A19. Version skew is invisible.** `ensure_running` reuses any answering server (`serve.rs:163`), so after an upgrade the chat talks to the old binary indefinitely. Compare `health.version` to `crate::VERSION`; mismatch means "restart the server" in the status line.

What held up: id validation (`valid_id`), path traversal in the store, 600/700 modes, `create_new` on result and session files, the rename-based result write, the `tools`-outside-workdir check, deny-on-dropped-reply in the chat, `prompt_sha256`, unknown methods, JSON depth (serde default limit), and `Client` dropping killing the `via` child.

## Part B: the plan

### H items that are right

- H2's connection and subscription caps: correct, and the subscription cap matters more than it looks because every `session.subscribe` is a thread. The `SO_PEERCRED` check is right in intent; the `serve.pid` file is the wrong tool (see below).
- H3's `shlex` and the doctor row: right, but incomplete (A10) and the "refuse a newline" clause is redundant once `shlex` parses.
- H5's size budget and header-only `list`: right. Add eviction of idle `Live` entries, otherwise memory grows with every session ever touched regardless of file size.

### H items that are wrong or over-engineered

- H1: the inbound `io::copy` into a `UnixStream` is not buffered; only the `StdoutLock` direction was. The real H1 items are the exit-code distinction (A15) and the client-side line cap (A5), not a second forwarding loop. An idle timeout belongs to the subscribing client, not the dumb bridge; ssh already has `ServerAliveInterval`.
- H2's `serve.pid`: a pid file is a second source of truth that goes stale on crash. A held `flock` on `serve.lock` fixes the bind race (A3), gives `stop` the pid via `fcntl(F_GETLK)` if ever needed, and is one `extern` instead of a file protocol.
- H4's "a server that dies mid-turn leaves the screen waiting": `follow` already maps `Disconnected` to `Failed { Transport }` (`service.rs:237-243`). The real gap is after that: the backend never reconnects, so every later turn fails until the chat is restarted. The fix is re-running `ensure_running` + `Client::open` on the next `submit` and continuing the same session id, which the server reopens from disk for free. The `detail` payload and native replay are features, not hardening; move them out of H.

### Missing H items (same table shape)

| Id | Path | Problem seen or foreseen | Change |
| --- | --- | --- | --- |
| H6 | serve | an `ask` emitted after the last subscriber left is never answered; a panic in a turn thread never closes the turn; both leave a session `conflict` until restart | emit-then-check in `handle`, drain `asks` as `Unattended` when subscribers hit zero, a server ask ceiling; a drop guard in `Turn::run` that closes the turn as `failed`; `exec::follow` deadline (A1, A2) |
| H7 | sessions | a torn tail is skipped on read but appended after on write, corrupting the file; two writers (server + import/standalone) collide on `seq` | truncate to the last complete line before the first append; `flock` the file while `Live` holds it; `import` refuses a held session (A4, A6) |
| H8 | client | the socket/stdio reader has no line cap; worker error text uncapped; worker-returned ids unvalidated | `MAX_LINE` on the client reader, caps on `error.message`, `valid_id` on `session` (A5, A17) |
| H9 | wire | `timeout`, `run_id`, `client` accepted unbounded; `Instant + Duration` panics | clamp and validate at `turn_request` (A8) |
| H10 | via | `SSH_AUTH_SOCK` scrubbed; stderr inherited into the chat; cancel waits the full worker timeout | transport allow-list, captured stderr, bounded post-cancel wait (A9, A11, A13) |

### Re-ordering

Hardening before skills. Three of the High items (A1, A2, A4) bite the chat and `exec` today without any attacker, and skills add a new tool result path that would inherit A1/A2. Proposed: H6 → H7 → H9 → H8 → H10 → H3 → S1 → S2 → H2 → S3 → S4 → H5 → H1 → H4-reconnect. H6/H7/H9 are each under a day and have no dependencies; H2's `flock` and `SO_PEERCRED` share the one `ahi-unix` PR and can land before S3 needs a stable server for its fixtures.

### Skills design, where it touches these paths

- **Workers.** "A planted skill is advertised only if the policy names it" holds only because `.ahi/skills` is under the workdir and `--tools` is outside it. Two gaps: a repo whose `ahi.tools.yaml` is the worker's policy (no `hc file`) approves its own skills; and a symlink `.ahi/skills -> /elsewhere` is refused by S2, but a symlinked `.ahi` directory above it is not mentioned; check the whole chain. The `PROTECTED` addition must also cover renames and `.ahi` itself being replaced.
- **Over `turn.submit`.** Agreed that no skill parameter is added. State one more invariant: `load_skill` enumerates roots relative to the *turn's* canonical `workdir`, never the session header's `cwd`, since `turn.submit.workdir` may differ (`service/mod.rs:407-410`); otherwise a coordinator picks a workdir to pick a skill set.
- **Operator root on a served host.** `<config>/skills` on the worker host is model-facing text that hc never placed and the coordinator never sees. Either drop the operator root for `origin: serve` turns or write the root into the `skill_load` entry *and* the result ledger so the coordinator learns a non-repo skill was read. The decision-2 "operator wins" rule becomes a hidden override on a worker.
- **`$name` in the chat.** Prefixing `Use the skill "name".` is operator text; fine. But it must not bypass A1: a skill under `ask` in a served chat with a dropped subscriber hangs like any other ask. Also `$name` with a name that no rule approves should be refused at the input line, not sent, or the record shows the operator asking for something policy denies.
- **Tolerant reader.** Skipping an unknown entry "in memory" breaks `seq` continuity (`store.rs:186`) and `path_to_root`'s `entries[seq-1]` indexing (`:297`). Keep a placeholder `Kind::Unknown { type }` at its index instead of skipping; otherwise parents resolve to the wrong entry silently. The header-schema refusal stays.
- **Budget.** Skill bytes count against the 256 KiB tool budget; in a served turn there is no per-turn wall clock for the chat (A1). A 64-skill root with 32 KiB each is 2 MiB the model can request one load at a time; fine with the budget, but `load_skill` results will cross the 256-event subscriber window faster than text does (A7), so land the `lagged` event first.
