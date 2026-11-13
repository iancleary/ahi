# 0.17.0: hardening the addressable service, then skills

Status: plan, revised after the audit in [AUDIT_0.16.md](AUDIT_0.16.md). The first pass put skills first and listed five hardening items from memory; the audit found three defects in 0.16.0 that hang or corrupt a session with no attacker involved, and corrected two of the five. This revision is what gets built, in this order.

## Decisions carried in

From [SKILLS.md](SKILLS.md), accepted by the owner, with the audit's amendments:

1. An operator skills root at `<config>/skills/<name>/SKILL.md` exists alongside the repository's `.ahi/skills`; the operator's name wins a collision. On a served host that root is text neither hc placed nor the coordinator sees, so every `skill_load` entry, ledger record, and audit line names the root (`repo` or `operator`), and a turn with `origin: serve` records the operator root's use as a distinct ledger record a coordinator can refuse to accept.
2. The session reader tolerates an entry type it does not know. It keeps the line at its `seq` as `Kind::Unknown { type }` so parents still resolve by index; `ahi session show` prints `unknown entry <type>`; `active_messages` skips it. A header schema newer than this ahi is still refused.

## Order

Hardening first: H6, H7, H9, H8, H10, H3, then S1, S2, H2, S3, S4, H5, H1, H4. The first three are each under a day, have no dependencies, and remove the hangs and the corruption. H2's two `extern` declarations (`flock`, peer credentials) land in one `ahi-unix` PR before S3 needs a stable server for its fixtures.

## Hardening

| Id | Path | Problem (audit anchor) | Change |
| --- | --- | --- | --- |
| H6 | serve | an `ask` emitted after the last subscriber left is never answered (A1); a panic in a turn thread never closes the turn (A2); both leave the session `conflict` until restart | in `Turn::handle`, emit the `ask` first and then test for subscribers, answering `Unattended` when none remain; when the last subscriber is pruned, drain `asks` as `Unattended`; a server-side ceiling on how long an ask may wait (10 min) regardless of client; a drop guard in `Turn::run` that closes the turn `failed` (`error.kind: turn`, "the turn thread panicked"), clears `open` and `asks`, and emits `turn`; `exec::follow` gets its own deadline of `timeout + GRACE + CANCEL_GRACE` and exits 6 naming the run |
| H7 | sessions | a torn tail is skipped on read but appended after on write, merging two lines and making the file `Corrupt` for every reader (A4); two writers (server plus `import` or a standalone `--resume`) collide on `seq` (A6) | `Session::read` records the offset of the last complete line and the first append truncates to it; a `flock` on the session file while a `Live` holds it, so `import`, a standalone resume, and a second server fail fast with `conflict`; idle `Live` entries evicted after the last subscriber leaves and no turn is open |
| H9 | wire | `timeout` is any `u64` and `Instant + Duration` panics the connection thread (A8); `run_id` and `client` are unvalidated and unbounded | clamp `timeout` to 3600 s and refuse larger as `usage`; validate `run_id` server-side as the client does; bound `client` to 64 bytes; the connection thread sets `closed` on every exit path |
| H8 | client | the socket and stdio reader has no line cap, so a hostile worker can exhaust a coordinator (A5); worker error text reaches the model uncapped; worker-returned `session` and `status` are unvalidated (A17) | `MAX_LINE` on the client reader with the connection dropped on overflow; `error.message` capped like `text`; `valid_id` on the worker's session, `status` restricted to the four values |
| H10 | via | `SSH_AUTH_SOCK` is scrubbed from the connect command, so `--via "ssh …"` and the default `hc connect` chain run without an agent (A9); stderr of the command is inherited into a raw-mode screen (A11); a cancelled delegation waits the worker's full timeout (A13) | a transport allow-list (`BASE_ENV` plus `SSH_AUTH_SOCK`, `SSH_AGENT_PID`) documented in SERVE.md; stderr captured on a thread, capped at 8 KiB, shown in the tool result on failure; after `turn.cancel`, wait `CANCEL_GRACE + 2 s` then return `effects: unknown` |
| H3 | dispatch | `AHI_DELEGATE_VIA`, `ahi exec --via`, and `ahi tui --via` are each split on whitespace (A10), so a path with a space breaks | one parser, the hook runner's `shlex`, in one place; the resolved argv in the audit line; an `ahi doctor` row showing the active template |
| H2 | serve | two chats starting at once can produce two servers and an orphan listening on an unlinked socket (A3); the socket accepts any same-user process; no connection or subscription limits; the socket is umask-mode between bind and chmod (A14); `stop` keeps accepting during the drain (A18); an upgraded binary keeps talking to the old server (A19) | an exclusive `flock` on `serve.lock` for the server's life (one `extern` in `ahi-unix`), the socket removed only while the lock is held, a second `run` exits "a server already runs" (no pid file); peer credentials checked (`SO_PEERCRED`/`getpeereid`, the other `extern`), another uid refused; caps of 64 connections and 16 subscriptions per connection; bind to a temporary name in the private directory and rename; a refusing flag during the drain (`conflict: shutting down`); the chat compares `health.version` with its own and says "restart the server" on mismatch |
| H5 | sessions | one file per session with no bound; `list` reads every file whole | a 64 MiB per-session budget after which new turns are refused with `limit`; `list` reads the header and scans turn lines without materializing entries |
| H1 | connect | exit 0 whichever side closed, so a coordinator cannot tell stdin EOF from a dead server (A15); the lagged-subscriber drop is silent and the client spins (A7) | exit 0 on stdin EOF, 3 when the server closed; a `lagged` event sent before a forwarder gives up, which clients treat as "resubscribe from the last `seq`" or fail. The inbound copy is not buffered and stays; an idle timeout belongs to ssh, not the bridge |
| H4 | tui as client | after a dead server the backend never reconnects, so every later turn fails until the chat restarts | on the next `submit`, re-run `ensure_running` and `Client::open` and continue the same session id, which the server reopens from disk. Client-mode `detail` payloads and native replay on resume are features and move to Later |

Smaller items from the audit folded into the PRs above: `inside()` resolves the nearest existing ancestor (A16, H3's PR); `ServiceBackend::cancel` fires on a thread and `turn.cancel`/`ask.answer` use a short call timeout so the screen never blocks on a wedged server (A12, H4's PR); `create_private_dir` refuses a pre-existing group- or world-accessible parent for the socket (A14, H2's PR).

## Skills (SKILLS.md, four PRs)

| PR | Scope | Audit amendments |
| --- | --- | --- |
| S1 policy kind | `action: skill`, `match.skill: <name>`, errors for the other matchers, `ahi tools list` and `ahi doctor` rows, `examples/tools/skills.yaml` | `$name` with a name no rule approves is refused at the input line, not sent |
| S2 loader | `src/skills/`: enumerate both roots, frontmatter, caps (32 KiB, 64 skills, 16 references), name equals directory, operator shadows repo; `.ahi/skills` joins `PROTECTED`; `ahi doctor` lists valid and rejected skills with reasons | the whole path chain `.ahi/skills/<name>` is checked for symlinks, not only the leaf; `PROTECTED` covers renames and `.ahi` itself; roots are enumerated relative to the turn's canonical `workdir`, never the session header's `cwd` |
| S3 `load_skill` builtin | schema with the approved list in its description; evaluated under `ActionKind::Skill`; `ask` prompt (name, root, bytes); session grant by name; budget accounting; reference files | lands after H6 so an `ask` on a skill cannot hang a served chat, and after H1's `lagged` event since loads cross the subscriber window faster than text |
| S4 record | `skill_load` entry (with `root`), `session show` line, ledger and result fields, audit line, `$name` input prefix; the tolerant reader as `Kind::Unknown` | |

## Not in 0.17

A Windows named-pipe listener; a registry or fetch for skills; `match.skill: "*"`; client-mode screen detail and native replay on resume; two screens typing into one live session; per-tool OSC 7501 child records.

## Tests the plan owes

- H6: a subscriber that disconnects before an `ask` leaves a turn that closes `cancelled`/`Unattended`, not open; a turn thread that panics closes the turn `failed` and the next `turn.submit` is accepted; `exec` exits 6 when no `turn` event ever arrives.
- H7: a torn tail followed by an append yields a readable file; `import` of a session the server holds is `conflict`.
- H9: `"timeout": 18446744073709551615` is `usage`, not a dead connection thread.
- H8: a 10 MiB line from a fake worker drops the connection and the delegation returns a tool error.
- H10: `--via` sees `SSH_AUTH_SOCK`; a worker that ignores `turn.cancel` returns within `CANCEL_GRACE + 2 s`.
- H3: a template with a quoted path resolves; the audit line carries the argv.
- H2: two `ensure_running` at once yield one server; a second `serve` exits with the lock message; a connection from a fake other uid is refused (decision unit-tested); a `turn.submit` during drain is `conflict`.
- H1: the bridge exits 3 when the server closes; a lagged subscriber receives `lagged`.
- H4: a chat whose server was stopped mid-session submits its next turn to a fresh server on the same session.
- Skills: every refusal in SKILLS.md; a planted `.ahi/skills` in a worker checkout invisible without a rule; a model write to `.ahi/skills` denied under a wide `file_write` rule; a symlinked `.ahi` refused; an operator-root load on a served host visible in the coordinator's ledger.
