# 0.17.0: skills, and hardening the addressable service

Status: draft plan, first pass. 0.16.0 shipped the service (`ahi serve`), the clients (`ahi exec`, `ahi tui`, `ahi connect`), recorded sessions, and delegation. 0.17.0 adds skills, as designed in [SKILLS.md](SKILLS.md), and hardens the paths that 0.16.0 opened: a socket that other programs call, a stdio bridge that crosses machines, a chat that is a client of a server it did not start, and a worker that runs tasks for a coordinator it cannot see. This document is the outline and the shape; an audit pass over it follows before anything is built.

## Decisions carried in

From [SKILLS.md](SKILLS.md), accepted by the owner:

1. An operator skills root at `<config>/skills/<name>/SKILL.md` exists alongside the repository's `.ahi/skills`; the operator's name wins a collision and the record says which root answered.
2. The session reader tolerates an entry type it does not know: the line is kept on disk, skipped in memory with one warning, and `ahi session show` prints it as `unknown entry <type>`. A header schema newer than this ahi is still refused.

## Shape

### Skills (SKILLS.md, four PRs)

| PR | Scope |
| --- | --- |
| S1 policy kind | `action: skill`, `match.skill: <name>`, errors for the other matchers, `ahi tools list` and `ahi doctor` rows, `examples/tools/skills.yaml` |
| S2 loader | `src/skills/`: enumerate both roots, frontmatter, caps (32 KiB, 64 skills, 16 references), symlink and traversal refusal, name equals directory, operator shadows repo; `.ahi/skills` joins `PROTECTED`; `ahi doctor` lists valid and rejected skills with the reason |
| S3 `load_skill` builtin | schema with the approved list in its description; evaluated under `ActionKind::Skill`; `ask` prompt (name, root, bytes); session grant by name; budget accounting; reference files |
| S4 record | `skill_load` entry, `session show` line, ledger and result fields, audit line, `$name` input prefix; the tolerant reader |

### Hardening (H1–H5)

What 0.16.0 taught, each with its fix. These are the items a second reviewer should push on hardest.

| Id | Path | Problem seen or foreseen | Change |
| --- | --- | --- | --- |
| H1 | connect | `io::copy` into stdout buffered a one-line answer until EOF on Linux (fixed in 0.16.0, by hand). The inbound copy still uses `io::copy`, and a bridge has no idle detection | one explicit forwarding loop in both directions; `ahi connect --timeout <secs>` idle limit for scripts; exit codes documented (0 peer closed, 1 no server, 2 usage) |
| H2 | serve | the socket accepts any same-user process; a stale socket is detected only by a failed connect; no limit on connections or subscribers per connection | peer credential check on Unix (`SO_PEERCRED`/`getpeereid`, one more `extern` in `ahi-unix`) refusing another uid; a `serve.pid` beside the socket so `stop` can signal a server whose socket is wedged; a connection cap (64) and a subscriptions-per-connection cap (16), both refused with `usage` |
| H3 | exec and dispatch | `delegate` reaches a worker through an argv template from the environment; the template is split on whitespace, so a path with a space breaks and a hostile environment variable could redirect every delegation | `AHI_DELEGATE_VIA` parsed with the hook runner's `shlex`; refused when it contains a newline; the resolved argv written to the audit line; an `ahi doctor` row showing the active template |
| H4 | tui as client | a client-mode tool result carries no `detail` (diffs, command streams), so the screen shows previews only; native replay items are dropped on `--resume`; a server that dies mid-turn leaves the screen waiting on a closed channel | `entry` events carry the screen detail as an optional `detail` object (bounded, never sent to a model); native items replayed when the protocol matches at resume; the backend maps a closed connection to `Failed { Transport }` with a resend offer |
| H5 | sessions | the store is one file per session with no size bound; a long-lived served session grows without limit; `list` reads every file whole | a per-session byte budget (64 MiB) after which the server refuses new turns with `limit` and says so; `list` reads the header and scans for turn lines without materializing entries |

### Not in 0.17

A Windows named-pipe listener; a registry or fetch for skills; `match.skill: "*"`; attaching to a remote session from the chat with shared live rendering between two screens (two clients already see the same stream; a second screen's keystrokes into one session is a UX question first); per-tool OSC 7501 child records.

## Order

S1, S2, H1, H2 are independent and can go first. S3 depends on S1 and S2; S4 on S3. H3 touches `delegate` only; H4 touches the backend and the wire; H5 the store. Proposed sequence: S1 → S2 → H1 → H2 → S3 → H3 → S4 → H4 → H5, each a draft PR on the previous where it depends, else on main.

## Tests the plan owes

- Skills: every refusal in SKILLS.md as a fixture; a planted `.ahi/skills` in a worker checkout invisible without a rule; a model write to `.ahi/skills` denied under a wide `file_write` rule.
- H1: the bridge returns a one-line answer with stdin held open, on Linux and macOS, in the integration suite (the case that failed in 0.16.0).
- H2: a connection from another uid is refused (a test that drops privileges is not available in CI; unit-test the decision with a fake credential).
- H3: a template with a quoted path resolves; a template with a newline is refused; the audit line carries the argv.
- H4: a client-mode `write_file` result shows a diff; resume replays native items for the same protocol.
- H5: a session at the budget refuses a turn with `limit`; `list` on a large store does not read bodies.

## Audit pass

Before building: a second reviewer reads this plan and the 0.16.0 code on the four paths (serve, connect, exec, tui) for security, reliability, and the failure modes of each, and reports what to change. The plan is then revised and the revision is what gets built.
