# Tool policy examples

Each file is a complete `ahi.tools.yaml` for the chat screen's builtin,
tools: the read-only `read_file`, `list_dir`, `find_files`, and `search_files`
(`action: tool_call`), the file-changing `edit_file` and `write_file`
(`action: file_write`), the program-running `run_command`
(`action: command`, matched on an argv prefix), and the task-handing
`delegate` (`action: delegate`, matched on a worker label), and the skill-reading
`load_skill` (`action: skill`, matched on a skill name). Write one into the working directory
with `ahi tools init --example <name> --write` (the examples are embedded in
the binary; `ahi tools examples list` names them and `ahi tools examples show [<name>]` prints them), copy one by hand, or point a session
at it without copying:

```sh
ahi tui --tools examples/tools/ask-every-read.yaml
```

Rule paths and tool confinement are always relative to the directory where
`ahi tui` started, not to the policy file. The file must exist and parse;
`ahi tui` refuses to start otherwise.

| File | What the model gets |
|---|---|
| `read-all.yaml` | Read, list, find, and search anywhere in the tree; the root `.env` is denied; no prompts. The repository root's `ahi.tools.yaml` starts with these rules. |
| `ask-every-read.yaml` | Every read, listing, find, and search asks in chat (`y` once, `s` for the session, `n` deny). |
| `sources-only.yaml` | Listing anywhere; reads, finds, and searches only under `src` and `docs`, with the root `.env` and `target` denied. |
| `list-only.yaml` | Listing only. The model can see the tree but reads are denied. |
| `edit-sources.yaml` | Reads anywhere; `edit_file` under `src` and `tests` asks first; `write_file` may create files under `tests` without asking; `Cargo.lock` is never written. |
| `commands.yaml` | Reads anywhere except the root `.env`; `cargo check` and `cargo test` run without a prompt, other `cargo` subcommands ask, `cargo publish` and `cargo install` are denied, any `sh` command run from `scripts` asks. |
| `delegate.yaml` | Reads anywhere; tasks may be handed to the workers labeled `rust-ci` (asks first) and `docs`; `prod` is denied. A label names a worker in herdr-core's registry, reached through `AHI_DELEGATE_VIA`, never a host. |
| `skills.yaml` | Reads anywhere; the skills named `rust-review` and `release` (asks first) may be loaded, `deploy` is denied. A skill is text under `.ahi/skills/<name>/SKILL.md`, advice to the model and never authority; nothing is advertised without a rule naming it. |
| `deny-all.yaml` | Valid, but no approve rule: no tool is advertised and any requested call fails with `tool execution is disabled`. |
| `invalid-version.yaml` | Deliberately invalid (`version: 2`): `ahi tui --tools` exits with the parse error, and as `ahi.tools.yaml` every turn fails closed. |

Rule paths are prefixes matched component by component, so `path: .env` names
the root file only; a nested `.env` or `.env.local` needs its own rule, and
`search_files` skips `.env` only when `.gitignore` does. Deny rules match
without regard to case and beat every approval.
