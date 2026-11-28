---
name: verify-live-harness
description: Exercise ahi interactively after changes to tool schemas, permissions, skill loading, AGENTS.md loading, capability summaries, mode selection, streaming, chat queue controls, or session exit. Verify model-chosen arguments, actual effects, and what the operator saw. Live inference requires separate authorization.
references:
  - command-cwd.md
---

# Verify a harness change with a real model

Pick the smallest probe that would distinguish the new behavior from the old. Do not run the whole matrix by default. Use a disposable workspace, the checkout's freshly built binary, and an explicitly authorized model, turn count, and surface. Never send real project files, credentials, or user conversations as test data. For a picker-only change, exercise the picker without submitting a model turn.

## Queue controls without live inference

For queue interaction, layout, or dispatch changes, build the checkout binary and run the maintained terminal workflow from the repository root:

```sh
cargo build --locked --bin ahi
uv run scripts/smoke-model-picker.py --binary target/debug/ahi --queue-manager-only --capture-directory target/chat-queue-captures
```

This probe uses disposable state and a loopback HTTP fixture in standalone and service-backed chat. It needs POSIX pseudo-terminals, local listeners, and Unix sockets. It does not need personal credentials or an external model service. Inspect the request-order assertions and the 80×24 and 40×12 captures. The controls and dispatch contract are documented in `docs/CHAT_QUEUE.md`.

If local listener creation fails with `Operation not permitted`, rerun with the required local execution permission. Preserve failure output and captures when an assertion fails. Correct the implementation or fixture, then repeat only this focused probe. The probe removes its disposable state and stops its own service. Do not restart an existing user service or broaden this check into paid inference.

## Model instruction context without live inference

For AGENTS.md loading, instruction precedence, capability summaries, or provider instruction encoding, build the checkout binary and run:

```sh
cargo build --locked --bin ahi
uv run --no-project --offline scripts/smoke-model-picker.py --binary target/debug/ahi --model-instructions-only --capture-directory target/model-instruction-captures
```

The POSIX fixture supplies disposable home, repository, policy, and state directories. Its loopback HTTP service captures all three provider encodings. It verifies standalone and service chat, next-turn refresh, fixed continuation snapshots, actual permitted effects, runtime denials, protected instruction files, invalid instruction files, and exclusion of instructions from durable history. Do not substitute a real home or credentials for this probe. No external model authorization is needed for the local fixture. See `docs/MODEL_CONTEXT.md` for the source and precedence contract.

Local listeners, Unix sockets, and pseudo-terminals must be permitted. Keep failure captures for diagnosis. The probe stops its own service and removes its disposable state; it does not restart a user service. After a failure, correct the implementation or fixture and rerun this focused workflow before considering live inference.

## Choose a probe

| Change under test | Ask or do in the UI | Independent evidence |
|---|---|---|
| `read_file` | Seed a file with a fresh marker **not in the prompt**; ask what the file says. | Recorded path and tool content contain the expected file name and marker respectively. |
| `list_dir`, `find_files`, `search_files` | Create nested files plus a decoy outside the requested root. Ask for a listing, a filename, or a distinctive line, naming the tool when testing its contract. | Inspect the selected tool, root, and result; check the expected relative paths and absence of the decoy. |
| `write_file`, `edit_file` | Ask for one new fixture file or one replacement in a seeded file. | Compare exact bytes on disk; for an edit, check untouched surrounding lines. A success sentence is not evidence. |
| `run_command` | Ask for one harmless process, no shell. | See [the cwd probe](command-cwd.md) for a runnable fixture and an offline call/result checker. |
| `load_skill` | Create a temporary `repo/probe` skill with a hidden marker. Ask to load it; for manual-only behavior, explicitly select it in the skills picker. | Match scoped ID and returned marker. Seeing a catalog description is not loading the body; reading it is not script execution. |
| `delegate` | Use only a specifically authorized disposable worker/connector. Without one, test refusal without connecting a worker. | Check the worker label and returned task/result or refusal. No remote execution claim without worker-side evidence. |
| Mode selection and enforcement | In `/mode`, inspect entries, move the highlight, cancel, then reopen and select. Compare the **same** write request in Read-only and Read-write using separate fixtures. For custom modes, choose a tool/path that the modes treat differently. | Check the displayed selection, draft preservation, actual file state and recorded calls/results. A queued mode must not change a running turn. Don't claim an unimplemented picker or mode works. |
| Ask/deny policy | Use a policy that asks for one harmless write. Decline it; with another authorized turn, accept in a fresh fixture. | Capture the approval interaction and compare disk state. If the model never calls a hidden tool, report “not invoked,” not “runtime denial verified.” Use a deterministic harness test for a forced denied call. |
| Streaming and exit | Observe a wrapping, multiline answer on the terminal under test. Quit normally. | Compare live screen, stdout after restore, and saved assistant message separately. A clean recording cannot establish a clean Rex redraw. |

## Run and report

1. Run `cargo build --locked --bin ahi` from the repo; save the binary's absolute path before changing directory. Seed only the chosen fixtures. For policy tests put a narrowly scoped `--tools` file **outside** the workspace. For workspace policy discovery, put the intended `ahi.modes.yaml` **inside** it instead. Use only schema supported by the binary under test.
2. From the fixture root, start the binary with `tui --standalone` (plus `--tools` if applicable). Keep recording enabled. Select the authorized model and intended mode. For service or Rex changes use that actual surface, explicitly noting the difference; don't substitute standalone or `exec` and call it an interactive pass.
3. Submit a natural-language request. Don't supply the JSON template or the expected hidden value. Observe picker state, approval prompt, streaming, and exit only where relevant. On failure, don't broaden permissions or spend another turn without authorization.
4. Inspect `ahi session show "$id" --json` with the same binary/state. Pair each assistant `calls[].id` with its tool `result.call_id`; inspect arguments, exit code and content/presentation. Compare against the fixture and the observed screen. Distinguish a malformed request, a policy decision, an execution error and a display-only failure.
5. Report binary version, model, surface, requested/active mode, fixture, session ID and separate verdicts for invocation, effect and display. Mark unobserved layers **not tested**. Keep captures local, then remove only the disposable fixture.
