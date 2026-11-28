# ahi

## Subagent design ([#123](https://github.com/ahi-sh/ahi/issues/123))

**ahi owns the subagent; Rex keeps it attachable; ahi’s durable session store keeps its conversation recoverable.**

- **ahi** admits each task, enforces the delegated mode against the workspace’s active permissions at admission, runs the child, and reports its status.
- **Rex** gives each child its own multiplexer session. Clients can attach and detach without owning or cancelling the run.
- **Durable history** uses ahi’s session recorder and JSONL store, with tool presentation records from `ahi-durable`. Rex scrollback is useful to an attached client, but it is not the authoritative transcript or proof that a run completed.

**Idle means ready for another turn, not closed.** In this design, the child ahi process stays at its prompt in the same Rex session until the orchestrator or the child explicitly closes it. A follow-up starts a new run in the same ahi conversation with a fresh permission check.

The child reports its state to Rex through the **Program Status Protocol (OSC 7501)**: `working` during a run, `blocked` when waiting for input, and `idle` when ready. `done` or `error` reports how the last run ended, not that the child exited. Clients can use Rex to see which child needs attention without parsing its screen. For the recorded outcome of a particular run, the orchestrator queries ahi by run ID.

## Delegation glossary

| Term | Meaning in ahi |
| --- | --- |
| **Delegation** | Asking another ahi worker to carry out a bounded task under an approved mode. |
| **Orchestrator** | The parent that delegates work, tracks its outcome, and decides whether to follow up, cancel a run, or close a child. The parent can keep chatting while the child works. |
| **Subagent (child)** | The ahi worker that does the delegated work. A resident child stays open between runs. Its **child ID** identifies it across those runs. |
| **ahi session** | The child’s recorded conversation: prompts, replies, tool calls, and results. A follow-up uses that history; closing the live child does not delete it. |
| **Rex session** | The multiplexer session that hosts the live child. Its **Rex session ID** tells a client where to attach, not which run to inspect. |
| **Run (assignment)** | One admitted piece of work, from acceptance to completion, failure, cancellation, or timeout. Its **run ID** identifies the work to inspect or cancel. For a local Rex child, this is the **assignment ID**. The first run uses the child ID; later runs have distinct IDs. |
| **Turn** | One prompt and the model and tool calls needed to answer it. Prompts queued together at admission can span several turns in one run. A follow-up submitted later starts a new run in the same ahi session. |
| **Admission** | The service’s permission check before accepting a run. The delegated mode cannot grant more than the caller or the workspace permits at that moment; a follow-up cannot widen the child’s existing permissions. |
| **Idle** | The child is open and ready for another run. Detaching disconnects a client; cancelling stops a run; closing ends the live child. |

For tool, policy, and terminal-host terminology, see the [general glossary](#glossary).

## Overview

`ahi` is a Rust library and command-line tool for working on software projects:

- **Terminal assistant:** `ahi tui` streams model responses from OpenAI and configured gateways, with workspace tools, saved sessions, and configurable execution permissions.
- **Offline hook runner:** `ahi precommit` reads [pre-commit](https://pre-commit.com/) configuration and runs builtin checks and approved local hooks. It does not download runtimes or clone hook repositories.
- **Development container:** `ahi dev` opens the working directory in a published image using Podman or Docker.

Tool permissions depend on the selected workspace mode. Define named modes in `ahi.modes.yaml`; existing `ahi.tools.yaml` policies remain supported. Keep only one of these files in a workspace. Commands run with the user's access; these permissions are not an OS sandbox.

## Installation

**Upgrading from `v0.28.0` or earlier?** The first `ahi-sh` release requires [one manual upgrade](MIGRATING.md#one-manual-upgrade-after-the-github-organization-transfer). Do not bypass an attestation failure in the old updater.

Every platform can install from crates.io with Rust 1.88 or newer from [rustup](https://rustup.rs/):

```sh
cargo install ahi --locked
ahi --version
```

Run the same command again to update. Linux, Windows, and Apple Silicon Macs can skip Cargo and use the prebuilt, attested archives below.

### Linux

Statically linked archives for **x86_64** and **aarch64** are on the [latest release](https://github.com/ahi-sh/ahi/releases/latest).

1. Download `ahi-<version>-linux-<architecture>.tar.gz` and `ahi-<version>-sha256sums.txt`.
2. [Verify the checksum and the GitHub build-provenance attestation](#verifying-downloads) before extracting.
3. Extract and install `ahi` somewhere on your `PATH`:

   ```sh
   VERSION=0.28.1 ARCH=x86_64   # or aarch64
   tar -xzf "ahi-$VERSION-linux-$ARCH.tar.gz"
   install -D -m 755 ahi "$HOME/.local/bin/ahi"
   ahi --version
   ```

Afterwards `ahi update` replaces the binary with the next release's archive, verifying the checksum file and the archive with the [GitHub CLI (`gh`)](https://cli.github.com/) before touching anything. It does not need Cargo.

### Windows

An **x86_64** archive is on the [latest release](https://github.com/ahi-sh/ahi/releases/latest).

1. Download `ahi-<version>-windows-x86_64.tar.gz` and `ahi-<version>-sha256sums.txt`.
2. [Verify the checksum and attestation](#verifying-downloads). In PowerShell, `Get-FileHash <archive> -Algorithm SHA256` gives the checksum to compare with the sums file, and `gh attestation verify` checks provenance.
3. Extract in PowerShell and put `ahi.exe` on your user `PATH`:

   ```powershell
   tar -xzf ahi-0.28.1-windows-x86_64.tar.gz
   .\ahi.exe --version
   ```

   The binary is unsigned, so SmartScreen may prompt on first run.

`ahi update` works here too and requires `gh`.

### macOS

On **Apple Silicon**, install GitHub CLI and run `gh auth login`. From the release checkout, run `bash scripts/install-macos.sh`. Or download the installer from the same tag you intend to install:

```sh
tag=v0.28.1
curl -fLsS "https://raw.githubusercontent.com/ahi-sh/ahi/$tag/scripts/install-macos.sh" -o install-macos.sh
bash install-macos.sh "$tag"
```

The script verifies GitHub attestations and the checksum before installing to `~/.local/bin/ahi` without sudo. Set `AHI_INSTALL_DIR` to override the destination. Run `command -v ahi` to check that your shell selects this copy, then use `ahi update` for later releases. On Intel Macs, continue using `cargo install ahi --locked`.

## Getting started

```sh
ahi doctor          # versions, the config, data, state and cache directories, and any mode warnings
ahi login           # sign in to OpenAI in the browser (optional)
ahi tui             # register a gateway with /gateway, curate models, chat
ahi --session <id>  # continue a saved chat (find IDs with ahi session list)
ahi precommit run --all-files # in a repository with .pre-commit-config.yaml
ahi dev             # shell in the published dev image, with podman or docker installed
```

Where ahi keeps `models.json`, the credential, and its logs is in [Files and directories](#files-and-directories). Installs older than 0.6.0 kept everything in `~/.local/share/ahi`; that section says what to move.

`ahi tui` shows only models you have curated. The quickest start is to save a `models.json` by hand in the config directory, `~/.config/ahi/models.json` on Linux and macOS (replace the example id with the model you want):

```json
{
  "providers": {
    "openai": {
      "baseUrl": "https://api.openai.com/v1",
      "api": "openai-completions",
      "models": [{"id": "gpt-6.1-sol"}]
    }
  }
}
```

For the hook runner, the `examples/` directory has ready-made configurations. `examples/starter` is builtins plus the two local Rust hooks, with a matching policy. `examples/ian-repos` is the same Rust hooks and the three pre-commit-hooks checks expressed as `repo: builtin`. Both expect:

```sh
ahi precommit run --fix
```

`examples/tools` holds chat tool policies (`ahi.tools.yaml`) from read-everything to deny-all, for `ahi tui --tools <file>` or copying into a repository; its README describes each.

`examples/starter/pre-commit` is a sample hook:

```sh
#!/bin/sh
exec ahi precommit run --fix
```

A repository can carry up to four ahi-related files, one per job. The two allowlists share one rule grammar (`id`, `action`, `match`, `effect`, optional `ask`), and the filename says which trust boundary a diff touches. Unknown keys are errors in every file, and the retired names `ahi.policy.yaml` and `ahi.actions.yaml` are refused rather than ignored.

| File | Decides | Read by |
| --- | --- | --- |
| `.pre-commit-config.yaml` | Which hooks run; upstream pre-commit format | `ahi precommit run`, `ahi precommit migrate` |
| `ahi.hooks.yaml` | Which local `system`/`script` hooks may execute, by id and exact entry (`action: hook`) | `ahi precommit run` |
| `ahi.tools.yaml` | Which builtin tools the model may call, where, and whether to ask: `action: tool_call` for `read_file`, `list_dir`, `find_files`, `search_files`; `action: file_write` for `edit_file`, `write_file`; `action: command` for `run_command` by argv prefix; `action: delegate` for `delegate` by worker label; `action: skill` for the skills the model may load, by name | `ahi tui` |
| `ahi.dev.yaml` | The development image, tag, shell, and runtime this repository prefers; grants nothing | `ahi dev` |

## Commands

Precommit commands now use `ahi precommit …`. The old `ahi run`, `list-checks`, `explain`, `migrate`, and `hooks install/uninstall` spellings remain temporary aliases that warn on stderr. See [MIGRATING.md](MIGRATING.md) for replacements, the support window, and Git shim guidance. The [module structure proposal](docs/MODULE_STRUCTURE.md) distinguishes this implemented slice from follow-up architecture work.

Every subcommand also accepts global flags: `-c/--config <path>` selects the config file, `--cache-dir <dir>` overrides `AHI_HOME` and `XDG_CACHE_HOME`, `--state-dir <dir>` overrides `AHI_STATE_DIR` and keeps every ahi directory under one root, and `--color auto|always|never` controls color, which only `ahi tools init` uses so far (`auto` colors a terminal unless `NO_COLOR` is set).

| Command | Summary |
| --- | --- |
| [`ahi precommit run [HOOK]`](#ahi-precommit-run) | Run the configured hooks on staged files, a file list, or a ref range, with text, JSON, or SARIF output. |
| [`ahi precommit list-checks`](#ahi-precommit-list-checks) | List the builtin check ids and aliases. |
| [`ahi precommit explain <id>`](#ahi-precommit-explain) | Describe one builtin check and the environment names local hooks receive. |
| [`ahi doctor`](#ahi-doctor) | Report versions, paths, directories, modes, and policy. |
| [`ahi terminal-host`](#ahi-terminal-host) | Explicit Rex/hc terminal execution and observation, with JSON output and side-effect-free `--dry-run`. |
| [`ahi precommit migrate`](#ahi-precommit-migrate) | Suggest an ahi config from an existing `.pre-commit-config.yaml`. |
| [`ahi precommit install`, `ahi precommit uninstall`](#ahi-precommit-install--uninstall) | Hook shim commands; not implemented, exit 1. |
| [`ahi update`](#ahi-update) | Replace the binary with the latest attested release archive. |
| [`ahi tools list`, `ahi tools examples [list\|show [<name>]]`, `ahi tools init [--example <name>] [--write]`](#ahi-tools) | Show the working directory's chat tool policy, show the shipped examples, or write one into the working directory. |
| [`ahi --session <id>` / `ahi tui [--refresh] [--session <id>] [--standalone] [--socket <path>]`](#ahi-tui) | Pick curated models, register gateways, and chat, with policy-gated read, search, edit, write, and command tools; turns run through `ahi serve`, started when needed, and every chat is recorded as a session. |
| [`ahi session list\|show\|export\|import`](#ahi-session) | List recorded sessions, print one, copy one out of the store, or bring one in. |
| [`ahi exec --model <gateway/model> (-p <text> \| --prompt-file <path> \| stdin)`](#ahi-exec) | One model turn from the command line, with tools under policy, a result document, and exit codes; the worker a coordinator runs over ssh. |
| [`ahi serve [start\|stop\|version]`, `ahi connect`](#ahi-serve) | Serve ahi's structured session protocol on a Unix socket; bridge stdio to that socket for `ssh host ahi connect`; `--via <command>` attaches a chat or a turn to a server elsewhere. |
| [`ahi login`](#ahi-login) | Sign in with ChatGPT in the browser. |
| [`ahi dev [build\|run\|shell\|status\|clean]`](#ahi-dev) | Open a shell, or run a command, in the published development image with the working directory mounted. |
| [`ahi sandbox check`](#ahi-sandbox) | Report the containment backend this host offers and whether an outer sandbox is already active. |

### ahi terminal-host

This operator-only interface controls external terminals, not ahi's TUI or agent scheduling. Like other operator commands, it does not consult `ahi.tools.yaml`; it does **not** add a model-facing tool. Model execution still uses the existing policy-gated tools. Avoid broad `run_command` approvals such as `[ahi]` if a model must not invoke this operator interface. Rex is the default backend; select hc with `--backend hc`. There is no automatic fallback, installation, or implicit focused-terminal targeting.

Preview a start without contacting either backend (replace the example IDs/paths with those of the selected execution environment):

```sh
ahi terminal-host --server work-wsl \
  --host-session session:11111111-1111-4111-8111-111111111111 \
  --dry-run start --cwd /workspace/project -- /usr/bin/env

ahi terminal-host --backend hc --host-session work-core --machine work-windows \
  --dry-run start --cwd 'C:\\work' -- 'C:\\Windows\\System32\\cmd.exe' /c ver
```

Remove `--dry-run` only when you intend to execute the request. Start requires an **absolute executable path and cwd on the target**; argv is literal, without shell expansion or hc template selection. A shell runs only if you explicitly name one. Rex needs an existing session and creates one terminal block in a containing window without requesting focus. hc starts without a parent, restart-on-loss, or parent notifications. Both leave terminal lifetime with the external server, not ahi's CLI connection.

Other subcommands are `inspect <terminal>`, `capture <terminal>`, `text <terminal> <text>`, `submit <terminal> <text>`, `key <terminal> <enter|escape|control-c>`, `interrupt <terminal>`, and `terminate <terminal>`. Repeat the same backend/server/core/machine selection for each operation. Use the full `block:UUID` returned by Rex, or hc's address (`t7` locally, `machine/t7` remotely), never a label or ahi session ID. Native Windows and WSL must have distinct targets; do not carry local hc handles between machines.

- hc `submit` pastes and presses Enter with its draft protection; raw `text` is unsupported. Rex `submit` writes text and then presses Enter as two sequential calls, so failure between them can leave partial input.
- Capture returns a bounded snapshot in JSON, not an output stream or structured agent response. hc preserves its completeness metadata; Rex's retained scrollback may already have lost older output.
- Interrupt does not imply exit: Rex sends Ctrl+C; hc uses its native stop-key protocol and returns what it observed. Terminate closes only the Rex block, never its entire session; hc ends the program and retains the record. A disconnect or missing process evidence is not reported as an exit.
- Calls have a whole-operation timeout of 15 seconds (`--timeout 1..120`), native stdout limits of 1 MiB and stderr limits of 16 KiB, and no mutation retries. A timeout or invalid response can leave **uncertain effects**; inspect the original target before retrying. Closing a connector is not containment of the hosted program. The hc CLI retains its native core-start behavior when invoked.

Success writes one JSON document to stdout. Diagnostics go to stderr. Exit codes: **0** success, **2** invalid input, **3** unsupported operation, **4** executable unavailable, **5** timeout, **6** response limit, **7** incompatible response, **8** native backend failure. `--help` and `--dry-run` never contact a backend. Native diagnostics are not echoed because they may contain command data.

The adapters have disposable fixture coverage against hc 0.1.21's schema and the reviewed Rex CLI/API shapes. **Live backend interoperability is not yet verified.** Neither backend was installed, started, or used to create terminals during these checks. Target configuration, streaming output, UI-client management, layout, focus, and orchestration are outside this slice.

### ahi precommit run

Text output is streamed to stdout: an immediate `ahi precommit: starting checks`
line, a `Starting:` line before each selected hook is filtered or executed, and
its completion line with `Passed`/`Failed` and an exit code (or `Skipped (not run)`).
Builtin checks and policy refusals use runner-equivalent exit codes 0/1; local
hook failures retain their actual exit code. Findings and captured hook output
follow that hook immediately, without waiting for later hooks. Local subprocess
output remains captured, not streamed byte by byte; `--verbose` still controls
output from passing hooks. A final summary counts passed, failed, and skipped
checks and reports ahi's exit code. Progress is flushed even through pipes, uses
no cursor control, and applies to the deprecated `ahi run` alias too.
`--format json` and `--format sarif` emit only their final structured document,
without progress banners. Errors and deprecation warnings remain on stderr.

Staged files. `[HOOK]` runs only that hook id or alias. `--all-files`, `--files`, and `--from-ref`/`--to-ref` select other sets, `--hook-stage` selects a stage other than pre-commit, and `--commit-msg-filename` feeds the commit-message path to commit-msg and prepare-commit-msg stages. `--format` is `text`, `json`, or `sarif`. `--fail-fast` stops after the first failing hook, and `--verbose`/`-v` shows passing local-hook output. `--fix` rewrites end-of-file, trailing whitespace, and mixed line endings.

Checkers are `repo: builtin`. `ahi lock`, `ahi verify`, `ahi fetch`, `self-update`, `install`, and `uninstall` are not commands. The hook shim commands are `ahi precommit install` and `ahi precommit uninstall`, and they exit 1. `repo: local` with `language: system` or `language: script` runs when an `ahi.hooks.yaml` rule approves that hook id and the exact `entry` string. The entry is split without a shell. A missing rule prints the snippet to add and does not run the command. Other local languages fail the hook and say `not supported by ahi`. `repo: meta` fails with an explicit error. A URL repo fails the hook and is not fetched. Nothing is skipped silently.

ahi runs tools that are already installed. It can pass a toolchain manager the variables it needs to find itself. It does not install, update, resolve, or cache a toolchain or dependencies for a local hook.

| | Names |
| --- | --- |
| Passed by default, if set | `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, plus `PATH`, `HOME`, and the other base names in the spec. `PRE_COMMIT=1` is set. |
| Policy only (`toolchain_env`) | `GOROOT`, `GOPATH`, `VIRTUAL_ENV`, `NVM_DIR`. Not enabled by default. `NVM_DIR` does not enable Node and does not run `npm install`. |
| Never passed | Names containing `TOKEN`, `SECRET`, `PASSWORD`, or `KEY`, and names starting with `AWS_`, `AZURE_`, or `GITHUB_`. Listing one in the policy is an error. |

`ahi precommit explain` prints those names. The audit line records the names passed to each local hook and does not record values. A global `toolchain_env` list applies to every local hook. A `toolchain_env` list on one rule applies only to that rule's id and entry.

Builtin checks are report-only unless `--fix` is passed. `--fix` rewrites only `ahi/end-of-file`, `ahi/trailing-whitespace`, and `ahi/line-endings`, then the hook fails because the file changed. A hook argument `--fix` does the same for those three. `--fix=no` on mixed line endings stays report-only. The write is a temp file in the same directory, fsynced, then renamed. Symlinks are refused. Paths outside the selected set are refused. A file with unstaged changes is left unchanged. ahi does not stash.

A configuration that enables every builtin check:

```yaml
repos:
  - repo: builtin
    hooks:
      - id: ahi/trailing-whitespace
      - id: ahi/end-of-file
      - id: ahi/dockerfile
      - id: ahi/github-actions
      - id: ahi/shell-strict
      - id: ahi/line-endings
      - id: ahi/shebang-executable
      - id: ahi/executable-shebang
      - id: ahi/no-commit-to-branch
      - id: ahi/check-xml
      - id: ahi/aws-credentials
      - id: ahi/vcs-permalinks
      - id: ahi/byte-order-marker
```

`ahi/byte-order-marker` reports a leading UTF-8 BOM and does not remove it. `ahi/aws-credentials` matches access-key id shapes and labeled secret assignments in the selected files. It does not read `~/.aws` or the environment. `ahi/no-commit-to-branch` defaults to `main` and `master`.

### ahi precommit list-checks

Builtin check ids with aliases. `--format` is `text` (default) or `json`.

The ids, aliases, and what each check reports are the same table `ahi precommit explain` draws on. The hook configuration example under [ahi precommit run](#ahi-precommit-run) lists every builtin id.

### ahi precommit explain

What one check reports.

It also prints the environment names a local hook receives by default; see the table under [ahi precommit run](#ahi-precommit-run).

### ahi doctor

Version, git, config path, the `models.json`, data, state, and cache directories (and whether the cache is writable), scratch directory, hook policy summary, the chat tool policy (`ahi.tools.yaml` in the working directory: present or absent, approve/ask/deny rule counts, and the tools it advertises), the container runtime `ahi dev` would use, and the sandbox backend `ahi sandbox check` detects.

It warns about any ahi directory or file that other users can read; see [Files and directories](#files-and-directories).

### ahi precommit migrate

Best-effort suggestion from `.pre-commit-config.yaml`. Prints a report in four parts, "Builtin equivalents", "Local hooks kept as repo: local", "Not ported" with a reason per hook, and the `ahi.hooks.yaml` rules the local hooks need, then the suggested config and a closing line that says the file was left unchanged. Does not guarantee feature parity or the same file scope. Does not write unless `--write` is set, and does not replace an existing file unless `--force` is also set. `ahi precommit run` does not use the mapping.

`ahi precommit migrate` is a reading aid, not upstream `migrate-config`. It does not rewrite `sha`, `python_venv`, or legacy stage names, and it does not fetch a `rev`. Hooks with an obvious builtin id are suggested as `repo: builtin`. `repo: local` system and script hooks are copied with the policy snippet they need. Everything else is listed under "Not ported" with a reason. A run-time substitution of remote hook ids was removed; native checks run only from `repo: builtin`.

### ahi precommit install / uninstall

Exit 1. They are the hook shim commands and are not implemented. Top-level `install` and `uninstall` are not commands.

### ahi update

Replaces the running binary with the latest release from the attested archives. Verifies the sha256sums file and the target archive against their GitHub build-provenance attestations with the `gh` CLI, checks the archive sha256, extracts the single `ahi` member, and atomically renames it over the running binary (moving the running executable aside first on Windows). Refuses any target it cannot verify. Covers linux-x86_64, linux-aarch64, windows-x86_64, and macos-aarch64; other targets refuse with a `cargo install ahi` pointer. Does not accept a package name and is not named `self-update`.

Installation and the manual verification steps are under [Installation](#installation) and [Verifying downloads](#verifying-downloads).

### ahi tools

In Policy mode, chat tools require an approving workspace policy (`ahi.modes.yaml` or legacy `ahi.tools.yaml`) or an explicit `--tools` file. Interactive Read-only and Read-write modes are separate choices; see [Workspace trust and execution modes](#workspace-trust-and-execution-modes). `ahi tools init` prints one of the shipped policies from `examples/tools` (default `read-all`: read, list, find, and search anywhere, with the root `.env` denied) and, like `ahi precommit migrate`, writes nothing until `--write`; an existing file is replaced only with `--write --force`. `--example <name>` picks another policy. `ahi tools init --interactive` lists the modes and their capabilities in a terminal, accepts a name or number (Enter selects `read-all`, `q` cancels), and previews the selection; add `--write` to save it. Interactive selection requires terminal input and stderr; scripts should use `--example`. The `permissive` example enables workspace reads and writes without prompts and approves commands through `sh`, `bash`, `pwsh`, `powershell`, or `cmd`; other executable prefixes remain denied. Those shells have the operator's host access and can bypass builtin file protections: this is not a sandbox. The interactive chooser requires typing `permissive` again to acknowledge this; explicit `--example permissive` is the headless opt-in. Skills and delegation still require explicit rules. `ahi tools list` shows the `ahi.tools.yaml` in the working directory with the same summary `ahi doctor` prints, or says it is absent. `ahi tools examples list` (or bare `ahi tools examples`) names the shipped policies with what each lets the model do; `ahi tools examples show` prints every one and `ahi tools examples show <name>` prints one. The file is written to the working directory, where the chat reads it and where its rule paths are resolved. The chat screen itself never writes `ahi.tools.yaml`: the model's tools cannot touch it whatever the rules say, and no slash command edits it, so the policy changes only through this command or an editor. `ahi doctor` reports the file and what it approves. In a terminal, `list`, `examples`, and `init` paint each rule by its outcome, green for approve, gray for ask, red for deny, with a legend; `--color never` prints plain text and the written file is always plain. Every builtin tool works inside the working directory: rule paths are relative to it, absolute paths, `..`, and symbolic links are refused before any read, write, or command, and `run_command` starts only in a directory inside it, so a rule's `path` narrows access and never widens it. This is not a sandbox: a program approved by a command rule runs on the host with the operator's access and can follow symbolic links, read files above the working directory, and run other programs, so command rules deserve the most care; `ahi tui` inside `ahi dev run` adds a container boundary.

### ahi tui

Pick curated models from `models.json`. Load is local-only; `--refresh` explicitly refreshes reference catalogs. Enter saves a selection and opens chat; configured OpenAI-compatible gateways stream real model responses. A pipe prints the load and catalog membership.

#### Picker and curation

Model catalogs and curated selections are separate. The main picker offers **only** `providers.<id>.models` from `models.json`, in saved order, for OpenAI and every gateway. An omitted list stays empty even after refresh. Reference catalogs never supply picker defaults; their curation view lets you explicitly choose the saved list. See [CHANGELOG.md](CHANGELOG.md) for the breaking gateway-loading change.

The curated picker includes OpenAI and every registered gateway. Provider groups follow the key order of the `providers` object in `models.json`. An implicit, unconfigured OpenAI group starts first; an explicit gateway reorder saves its position too. Highlight a **provider/gateway header** and use **Shift+Up/Down** or **Shift+J/K** to move the whole group and save its order, without changing its models or credentials.

Models, Catalog, gateway setup, and login use plain headings and whitespace rather than box-drawing borders. Long values get the full content width. **`y`** copies the complete selected model ID in either model screen; **Ctrl+Y** copies the complete URL in gateway setup or login. These app-managed copies preserve original text without visual wrapping or UI decoration; terminal-native drag copying still depends on the terminal.

Press **`/`** in **Chat, Models, or Catalog** to open the same **Commands** popup. **Add gateway** (`/gateway`) is the first action, followed by model choice, catalog browsing, OpenAI sign-in, and Quit. New installations show this onboarding path explicitly. Gateway setup guides you through **Connect → Review → Choose models**; registration results and cache warnings appear in the destination catalog immediately. Type `/login` for ChatGPT sign-in without configuring a model first. `r` refreshes catalogs and `c` opens a provider's curation view. Checked models appear first in saved order, followed by unchecked catalog IDs. **Up/Down or j/k** moves the highlight. **Shift+Up/Down or Shift+j/k** reorders the highlighted checked model and saves. **Space** checks/unchecks and immediately saves. Configured IDs missing from the catalog remain checked and removable. **Enter** on a checked catalog model saves the active selection and opens/resumes chat; an unchecked model first needs **Space**. `y` copies an ID (or prints it on exit if no clipboard tool succeeds), and Esc returns to Models with the relevant provider and checked model still highlighted. Each curated ID shows `listed`, `absent from catalog`, or `catalog unavailable`.

#### Chat

**Enter** on a curated model saves the selection and opens chat. A configured gateway is called at `<baseUrl>` plus the path its `api` implies: `/chat/completions` for `openai-completions`, `/responses` for `openai-responses`, `/messages` for `anthropic-messages`, streaming the conversation history. This sends your messages to the selected provider and may incur charges. `apiKey` and custom header values resolve the marker `gateway` or `$ENV_NAME` at submission time; secrets are not stored in `models.json`. Curated OpenAI models use browser-login credentials with `POST https://api.openai.com/v1/responses`. Setting `ANTHROPIC_API_KEY` adds a built-in `anthropic` provider that talks to `https://api.anthropic.com/v1/messages` with that key. Every protocol validates streamed function calls and runs the policy-gated builtin tools described below through one turn loop. Model output is not execution authority. Invalid/incomplete calls fail closed; tool arguments are not printed in notices or parser errors.

#### Model instructions

Each turn loads optional operator `~/.agents/AGENTS.md` and repository-root `AGENTS.md` instructions. The model also receives a summary of its selected mode, available tools, command prefixes, and path restrictions, generated from the effective policy. Repository guidance overrides operator defaults for project matters; permission checks remain enforced by the mode and policy. Instructions stay fixed through a turn and refresh on the next one. See [Model instructions and capabilities](docs/MODEL_CONTEXT.md) for precedence, file limits, service behavior, and verification.

#### Skill completion

Type `$` in the chat input to open the skills picker. Search by skill name or
scoped ID; **Tab** cycles **Both → Repo → Operator**, **↑/↓** moves the highlight,
and **Enter** inserts a selection without sending. **Esc** leaves the query as
ordinary text, and bracketed paste never opens the picker. Only a confirmed
selection—not text like `$PATH` or `$repo/review`—requests an explicit load.

The same turn input is available in `ahi exec`, including headless runs, via
repeatable `--skill repo/review` flags. The shared backend loads the selected
skills **before the first model request** and records the loads as tool results.
Duplicate IDs load once per turn. A failed or denied explicit load stops
inference; headless runs deny loads requiring attendance.

Repository `.agents/skills` and operator `~/.agents/skills` definitions are
separate even when their names match. Policy names the exact scoped ID, such as
`match: {skill: repo/review}`. `ahi doctor` lists both roots. Portable metadata
does not grant tool permissions; manual-only skills require explicit selection.
See [Shared skill runtime](docs/SKILLS.md) for installation, metadata, and path
boundaries.

#### Workspace trust and execution modes

New workspaces open in **Plan** without a prompt. Plan can read the checkout, inspect Git status/diff/log, and keep one private temporary draft. It cannot edit the checkout, run commands or scripts, or delegate. **Trusted** enables those actions with your account's access, not in a sandbox; selecting it asks for trust unless already remembered. A workspace policy takes startup precedence (`default_mode` in v2, **Policy** in v1).

**Full Access** is Trusted plus file tools on absolute host paths (`read_file`, `list_dir`, `find_files`, `search_files`, `edit_file`, `write_file` accept `/absolute/paths` spelled exactly as on disk). It is only reachable from `/mode`, asks for confirmation every time, is never remembered, and Alt+M never cycles into it. Symbolic links, ahi's own directories, protected files and output caps are enforced the same way outside the workspace; commands keep a workspace working directory.

**Shift+Tab** cycles thinking effort (off, minimal, low, medium, high) without changing permissions. Ctrl+T still cycles effort. **Alt+M** cycles execution modes without changing your draft. **`/mode`** searches built-in and workspace choices. Selecting Trusted bypasses custom rules without changing the policy file. Changes during generation apply to the next turn, not an already running command. While a command runs, its stdout appears in one updating transcript block. The final result replaces that block; Ctrl+O expands captured stdout/stderr and the full write diff. **`/mode revoke`** forgets workspace trust. See
[Workspace trust](docs/WORKSPACE_TRUST.md) for persistence, service boundaries,
and headless behavior.

#### Tools and policy

In Policy mode (also the noninteractive `ahi exec` default), tools are off unless the working directory has an `ahi.tools.yaml` rule that approves one, for every provider. Only builtin tools exist, compiled into ahi and run in process with no `rg`, `fd`, `git`, or shell subprocess. Read-only, under `action: tool_call`: `read_file` (whole or a window of lines, with a continuation offset when the 64 KiB cap is hit), `list_dir`, `find_files` (glob over names or paths, honoring `.gitignore`), and `search_files` (regular expression or literal, `path:line:text`). File-changing, under `action: file_write`: `edit_file` (replace text that occurs exactly once, or every occurrence with `replace_all`) and `write_file` (create a file, or replace one with `overwrite`). A `file_write` rule names `edit_file`, `write_file`, or neither for both, so creating files and editing existing ones are approved separately. Task-handing, under `action: delegate`: `delegate` gives a task to a worker named by label (another `ahi serve`, reached through an explicitly configured `AHI_DELEGATE_VIA` command speaking ahi's session protocol; there is no default connector) and returns its answer, status, and `effects`; a rule approves a label with `match.worker` and never names a host. Skill-reading, under `action: skill`: `load_skill` returns the text of a skill (`.agents/skills/<name>/SKILL.md` in the working directory, or `~/.agents/skills/<name>/SKILL.md` on the operator's host (using the OS-resolved home directory, independent of ahi config/state overrides)) as a tool result with the authority of any file read, none; a rule names one scoped skill ID (`match.skill`) or explicitly opts in to `operator/*`, deny-by-default, and the tool is advertised only when an approved skill exists and validates, with approved automatic skills and explicitly selected manual-only skills listed in its description. Program-running, under `action: command`: `run_command` takes an argv list and a working directory, spawns no shell, and is approved by `match.argv`, a token-by-token prefix (`[cargo, test]` approves `cargo test --all-features`, not `cargo publish`); it runs with the hook runner's scrubbed environment plus the rule's `toolchain_env`, returns the exit code and capped stdout and stderr, and is killed at its timeout or when the turn is cancelled, together with every process it started (the program leads its own process group on Unix). Commands run on the host in this alpha; run `ahi tui` inside `ahi dev run` for a container boundary. All tools use software path checks within the working directory, never follow symbolic links, exclude ahi's own directories, and return bounded output; writes are atomic, append a hash-only line to `audit.log`, and can never touch `ahi.tools.yaml`, `ahi.hooks.yaml`, `ahi.dev.yaml`, `.pre-commit-config.yaml`, `.gitignore`, `.git`, `.agents` or `.ahi` itself, or anything under `.agents/skills` or the retired `.ahi/skills` (so a model cannot author a skill it later loads). The approval prompt for a write shows the path and line counts, never the text. These in-process checks are not an OS sandbox. A rule with `ask: true` shows an approval prompt in chat that lists the answers for each call: move the highlight with **↑/↓** and apply it with **Enter**, jump to another numbered call with **Shift+↑/↓**, or press the answer's letter (**y** once, **s** for this session under the shown directory and destination, **n** deny, **a** approve the rest, **Esc** deny the rest). Dismissing the prompt cancels the whole batch, including calls a rule had approved. Model output and tool results never grant authority. In Policy mode, without the file no `tools` are sent and requested calls fail with `tool execution is disabled`. Interactive Read-write and Read-only modes are separate explicit choices; see workspace trust above. `ahi tui --tools <file>` uses that policy file instead of the working directory's `ahi.tools.yaml`; it must exist and parse, and its rule paths still apply to the working directory. `examples/tools` holds ready-made policies for trying the tools. Approved file contents are sent to the selected provider. Invalid/incomplete calls fail closed; raw tool arguments are not printed in notices or errors.

```yaml
# ahi.tools.yaml
version: 1
default: deny
rules:
  - id: read-sources
    action: tool_call
    match: {tool: read_file, path: src}
    effect: approve
    ask: true
  - id: no-env
    action: tool_call
    match: {tool: read_file, path: .env}
    effect: deny
  - id: edit-sources
    action: file_write
    match: {tool: edit_file, path: src}
    effect: approve
    ask: true
  - id: cargo-test
    action: command
    match: {argv: [cargo, test]}
    effect: approve
```

#### Requests and limits

Token usage comes only from reported responses. Known catalog context capacity feeds the context meter; complete price/usage information enables cumulative reported-usage cost in Commands, not a claim about the provider's billing total. Reasoning effort is sent only when model metadata explicitly permits it and thinking is not Off. ahi passes the model’s declared `maxTokens` unchanged. Without one, Chat Completions and gateway Responses leave the output limit to the provider. Browser-login Responses always omits that field. Anthropic Messages requires `max_tokens`: refresh the catalog to pick up the provider’s limit, or set `maxTokens` on the model in `models.json` if the catalog omits it. If a provider cuts a reply short, streamed text stays visible; ahi neither retries automatically nor executes any tool calls from the cut-off reply. Exact serialized request-byte limits are enforced before submission.

Only one turn runs at a time. Enter during an active reply queues a FIFO follow-up. **Alt+Q** opens the queue manager above the composer. Select, edit, replace, reorder, remove, pause, or resume pending prompts without changing the composer draft. **?** shows the controls. Failure or Esc cancellation pauses the queue until explicit resume; `/clear` drops it. Pending prompts are session-local and are not restored after exit. See [Chat queue](docs/CHAT_QUEUE.md) for controls, dispatch behavior, limits, and verification.

Requests refuse redirects and cap events at 1 MiB and the total stream at 16 MiB. Interactive requests have no total timeout: generation may take as long as the provider needs, with only connection setup bounded to 30 seconds. A gateway that accepts a connection but never starts responding fails within the 2-second read poll; the request may still have been processed, so it is not retried. A gateway that cannot be reached (DNS or connection failure) gets three connection attempts with short pauses; generation is never automatically retried. After a connection failure, blank **Enter** offers to resend only when the last conversation row is your prompt—not after a partial reply or tool result. If a tool already ran, inspect its effects and send a new prompt. If the service loses the submit reply or the event connection, check the reported session and run ID before retrying; losing the connection is not proof the work stopped. OpenAI can renew once and resubmit after a definitive pre-stream HTTP 401; ambiguous failures or failures after content never retry generation. **Esc** cancels a streaming response: the partial reply is kept and sent with the next message, and the connection is closed within the read poll even while the stream is silent. Exiting chat does the same without the notice. Neither can make a provider stop generating before it notices the closed socket.

#### Commands popup

In the chat composer, **↑ / ↓** move through wrapped or multiline text first. At the top or bottom they recall older or newer inputs, including slash commands. **↓** past the newest entry restores the unsent draft and its cursor; submitting a recalled entry also returns you to that draft. **Ctrl+U** stashes what you’re typing without sending or executing it. Press **↑** to get it back, even after sending something else. Recall keeps up to 64 entries of up to 64 KiB each in memory for this open chat only. `/clear` and closing the chat forget them. Open menus and approval dialogs keep their usual arrow controls.

**Ctrl+R** searches these same entries, newest first. Type part of an input; press Ctrl+R again for an older match. Enter copies the match into the composer for editing, **not** submission; Esc or Ctrl+G cancels and keeps the draft and cursor untouched.

If a completed reply starts with literal `<tool_call>` markup and that turn received no structured calls, chat keeps the original answer and adds a notice explaining that no tool ran. The notice also appears in the conversation printed on exit; it is not a policy denial or an automatic retry.

During generation the chat normally shows **Thinking...** rather than painting each assistant token. The answer appears when the assistant finishes or requests tools. **Ctrl+O** lets you inspect the live reply and expanded tool results, then collapse them again. Approvals, command progress and errors remain visible. Interrupted or failed replies retain the text already received; the compact view does not change recording, provider requests or `ahi exec`.

The shared **Commands** popup opens immediately when you type **`/`**. Keep typing to filter by command, alias, or action label; use **Up/Down** to select, **Enter** to apply, and **Esc** to dismiss without executing. Chat retains its draft on dismissal; Models/Catalog return to list navigation. Single-line command paste in Models/Catalog is bounded and never submits by itself. Chat additionally offers thinking levels, mouse mode, and conversation clearing. Commands are local actions, never model prompts, and exact typed commands continue to work. During an active response, Enter preserves most command drafts; press Enter again after the response finishes to apply them. `/quit` and `/mode` controls remain available, with mode changes queued for the next turn. **`$`** opens the separate skill picker, not Commands.

**`/models`** (alias **`/model`**) reopens the curated picker across all providers with locally reloaded settings; selecting a model keeps the transcript, and cancelling keeps the active model. **`/catalog`** opens the active provider's full cached catalog for curation, including uncurated models; `r` explicitly refreshes it, Esc returns to the all-provider picker, and `q` returns to chat. No catalog is fetched just by opening it. **`/quit`** (alias **`/exit`**) closes ahi, even during an active response; upstream cancellation is not guaranteed. `/quit` also exits from a nested Models or Catalog screen; it is different from `q`, which cancels the picker and returns to chat when one exists. At the initial picker, `q` exits even if an old selection exists. **Ctrl+D** on empty chat input exits.

#### Clipboard and selection

Chat has mouse capture **on by default**, with wheel scrolling, clickable jump controls, and app-managed text selection. **Left-drag transcript text** to select it with a background highlight; releasing the drag copies it to the clipboard. **Ctrl+Y** copies the retained selection again; **Esc** clears the highlight without clearing your draft, and with no highlight it cancels a streaming response. Copy feedback appears in the input rule, never in the model transcript. On native Windows, ahi writes the system clipboard through the Win32 API, falling back to `powershell.exe` reading a private UTF-8 temporary file if the clipboard stays busy. On Linux/WSL, ahi tries display-appropriate clipboard tools first (`wl-copy` for Wayland, `xclip`/`xsel` for X11). If those fail on WSL, Windows Terminal receives a bounded OSC 52 copy request; other WSL terminals fall back to `wslpath` and `powershell.exe` using a private UTF-8 temporary file. Headless Linux and SSH/mosh sessions also use OSC 52; set `AHI_CLIPBOARD_OSC52=1` to request it for remote clients without SSH markers. OSC 52 requires terminal support and cannot acknowledge acceptance; its base64 payload is limited to 100,000 characters. If no clipboard adapter succeeds, the selection stays highlighted for retry. **Ctrl+S** (or **`/select`**) enters a keyboard selection mode over the transcript: **h/j/k/l** or the arrows move a cursor, **w**/**b** move by word using Vim's character classes (keyword, punctuation, emoji, and one class per CJK script, so `foo.bar` is three words and a run of ideographs is one), **0**/**$** jump within the row, **g**/**G** jump to the oldest/newest row, Page Up/Down move by a page, **v** starts a character-wise visual range, **V** a line-wise one over whole logical lines with soft wraps rejoined and a trailing newline on yank, exactly as in Vim, **y** yanks it to the clipboard and leaves the mode with the highlight kept for a Ctrl+Y retry, and **Esc** (or Ctrl+C) leaves without copying. The transcript scrolls to keep the cursor visible, selection keys never reach the draft, and the mode works with mouse capture on or off. Assistant replies render markdown (headings, emphasis, code blocks with syntax highlighting and a `copy` affordance, lists, tables); `/plain` shows the source. Tool results show a header with the call's duration, a red/green unified diff for edits and writes, and labeled `stdout`/`stderr` blocks for commands. URLs and workspace file references are underlined: click one (or press **Enter** on it in keyboard selection) to open the URL in your browser or the file in your editor (`/editor code|cursor|zed|subl` or an argv template; `$VISUAL`/`$EDITOR` and `code` on `PATH` are the defaults). Selections copy rendered text; **Y** or `/copy raw` copies the markdown source. A finished turn ends with `done in 12s · 14:02` (`/clock utc` for UTC). Native terminal **Cmd+C** is not the app's copy shortcut; use **`/mouse off`** if you prefer terminal-native drag selection and copying. You can also scroll the chat transcript with **Page Up / Page Down**. **Ctrl+K** (or **Ctrl+Home**) jumps to the top; **Ctrl+L** (or **Ctrl+End**) jumps to the bottom and resumes following streamed output.

Use **`/mouse on`** to re-enable application mouse handling: the wheel scrolls the transcript, and floating **Top**/**Bottom** controls support hover and click. Wheel scrolling preserves selected transcript rows, and click-without-drag still activates the jump controls. Copying rejoins soft-wrapped rows while preserving original newlines and spaces, and handles wide Unicode characters and combining marks. Resizing the transcript width, clearing chat, or changing mouse mode clears selection. Dragging onto the top row or below the transcript auto-scrolls one row per frame while the button is held, extending the highlight from the original anchor into newly revealed text; moving back inside the viewport or releasing stops it. The wheel still extends a selection too. **`/mouse off`** restores native selection and hides those controls; **`/mouse`** reports the current mode. The mode is session-local and starts on each time you launch chat. Scrolling up keeps your reading position fixed as new output arrives. Plain Home/End still move within the input, and Ctrl+J still inserts a newline.

#### Editing models.json

A starter `models.json` is in [Getting started](#getting-started). Explicit checkbox/reorder saves atomically replace `models.json` in the config directory, pretty-printing JSON while preserving other providers and settings. Gateway reordering intentionally changes provider key order; model curation preserves it. Stale external edits or save failures leave the file and loaded list unchanged; press `e` to review/reload before retrying. Merely browsing does not create a file; an explicit checkbox action can.

`e` opens `models.json` in `$VISUAL` or `$EDITOR` (notepad fallback on Windows). Other systems ask for an editor when neither is set. Editor-only reload never rewrites the editor's bytes and never fetches. Invalid edits retain the previous registry and display the file path and line.

#### Catalogs and refresh

Explicit OpenAI refresh requests `GET https://api.openai.com/v1/models` with direct-token credentials. It accepts `models[].slug` and the standard `data[].id` format, preserving reported IDs in order. A 403 preserves the catalog and curated IDs and reports unavailable discovery. Expiry is checked before requests; authentication failures permit at most one renewal and retry. Requests refuse redirects, have a 15-second timeout, and cap responses at 1 MiB. `auth signed-in` means only that a credential file exists.

Gateway catalog refresh requests `GET <origin>/agent/models.json`, stripping the path from the configured `baseUrl`. Both the original flat catalog and supported Pi-shaped `{"providers": {"home": {...}}}` documents are accepted. A validated returned same-origin `baseUrl` supplies the inference path for new registration and is shown in Review; no `/v1` is guessed. Multiple candidates require an exact inference URL to disambiguate. Metadata survives caching and curation; discovery never automatically selects remote models. This is a gateway-specific discovery convention, not an OpenAI or industry-standard catalog endpoint. The gateway retains `/pi/agent/model.json` as a legacy alias; ahi uses only the canonical route. Gateways without this catalog route can still serve inference for explicitly configured model IDs.

`ahi tui` never fetches on startup. `ahi tui --refresh` explicitly requests refreshes, preserves old catalogs on failure, and in a pipe exits 1 if any refresh failed while still printing the local load. Catalogs are atomically replaced objects containing provider, source URL, retrieval timestamp, ordered reported IDs, and typed metadata by model ID; old array-format and identifier-only object catalogs remain readable with unknown metadata. `retrieval time unknown (old catalog format)` means that the cached array lacks a timestamp, not that authentication is old. Refresh never edits the curated file.

#### The chat and the server

`ahi tui` checks the local service protocol before opening the interactive model picker or a resumed chat. `health` reports both the ahi release (`version`) and the session protocol (`protocol_version`, currently `1`). A newer client can reuse an older daemon that speaks the same protocol, including on reconnect and for local `ahi exec` turns. Optional features still require their advertised capabilities. The daemon continues running its original code until you restart it. To compare the client with the running daemon, use `ahi serve version` (from a checkout: `cargo run -- serve version`). It accepts `--socket <path>`, shows `not reported` for legacy daemons, and exits with an error if none answers; it never starts or stops one.

If a daemon predates `protocol_version` (including `v0.28.1`) or advertises an incompatible protocol, the client reports the mismatch without stopping it. When active turns are finished, run `ahi serve stop` (with the same `--socket` if applicable), then `ahi tui`. The first upgrade from an unversioned daemon needs this restart once; ordinary compatible upgrades do not. When no `ahi serve` answers on the socket (`$XDG_RUNTIME_DIR/ahi/serve.sock`, else `<state>/run/serve.sock`; `--socket <path>` names another), the chat starts one detached, with its output in `<state>/serve.log`, waits up to two seconds for it to answer, and attaches. Two chats started together end up on one server. The server outlives the chat and never idles out, so the next chat, `ahi exec`, or another ahi-protocol client can find it running; `ahi serve stop` ends it. In this mode the server records the session and the screen is a client: the same transcript, tools, approval dialog, `/model`, `/thinking`, and `--resume`, with the approval answered over the socket. `--standalone` runs the engine inside the chat as before and records the session itself; `--no-history` applies there. On Windows the chat is always standalone.

**Developing the TUI:** Run `just tui` from the checkout (or use
`cargo run -- tui --standalone` without `just`). This runs the checkout's engine
inside the TUI without connecting to, starting, or restarting the installed
session service. It still uses your normal ahi configuration and session store.
On Unix, run `just tui-dev` to test the client/server path without using your
installed configuration, sessions, or daemon. It uses `~/.local/state/ahi-dev`
and an explicit socket at `~/.local/state/ahi-dev/run/serve.sock`. This state
persists across dev runs; configure models there separately. For isolated state
without a daemon, use `cargo run -- --state-dir <dir> tui --standalone`.

#### Chat appearance

Ahi reads two optional, operator-owned TOML files from its config directory on TUI startup. On Unix this is `$XDG_CONFIG_HOME/ahi`, or `~/.config/ahi`; `--state-dir` overrides it along with the other ahi directories. These files are not read from the workspace or sent to a model. Restart the TUI after editing them; sessions use the current local appearance rather than storing a theme.

`padding.toml` sets space **inside colored message blocks**, not around the overall terminal UI:

```toml
horizontal = 1 # terminal columns on each side
vertical = 1   # terminal rows above and below
```

Ratatui measures terminal cells, not CSS `em` units. Values are integers from 0 to 255; zero disables an axis. Horizontal padding shrinks in very narrow terminals so content can still render. These are application-level transcript settings; `ahi-widgets` remains independent of config files.

`theme.toml` controls message backgrounds and edit-diff colors. All keys are optional. Colors accept `"#RRGGBB"` or `"default"` to use the terminal default:

```toml
[backgrounds]
operator = "#1e2d3c"
assistant = "default"
tool_success = "#1e3c2d"
tool_error = "#3c1e1e"
alert = "#463214"
notice = "default"

[diff]
added = "#a0e6aa"
removed = "#ffa0a0"
added_background = "#23552d"
removed_background = "#5a2323"
added_emphasis = "#328241"
removed_emphasis = "#8c3232"
```

Missing files or keys use these defaults without writing configuration. Invalid colors, unknown keys, and invalid padding values stop startup with the filename and parse error. Complete examples are in `examples/tui/`. Menu colors and syntax-token palettes are not configurable through these files yet.

Edit previews show all generated unified-diff hunks with old/new line numbers, red removed text, green added text, and stronger backgrounds for changed words. Long diff lines wrap instead of being clipped at 400 characters; previews no longer stop at 200 rows. Ctrl+O shows the local capture, not the exact bytes sent to the model. When the turn’s tool-output budget cuts the model-facing result, the collapsed and expanded headers say so. `run_command` reports the subprocess exit code in new tool records and in `exec -v`; an approved program that exits 126 is a failed command, not a policy denial. Search results JSON-quote paths containing colons or quotes; ambiguous historical rows stay readable without a guessed file link. Command-output and read-preview limits are unchanged. Tool presentation is recorded with the session: edit before/after text, command argv and separated output streams, search/read metadata, exit codes, and durations survive resume and export/import. The service-backed live TUI uses the same records. Older sessions without presentation metadata still load with their original plain previews. Presentation data is not sent to the model.

Each serialized tool detail is limited to 512 KiB to leave room for the bounded result text within the service's 2 MiB frame limit. Larger previews display an explicit omission explanation instead of a partial or misleading diff; the result and duration remain recorded. This applies in standalone and service-backed chat. Session files now retain more source/output data and can reach their existing 64 MiB budget sooner; keep exported sessions private.

### ahi session

Every chat is recorded as it happens into one file of JSON lines under the state directory, `sessions/<id>.jsonl`, created owner-only at the first turn: the operator's message before the request goes out, the model's reply and its tool calls before any tool runs, each tool result after, and one `turn` line per exchange saying how it ended (`ok`, `failed`, `cancelled`). `/model` and `/thinking` are recorded too. Nothing is rewritten: a failed or cancelled turn stays on disk as a branch, and the next turn hangs off the last turn that succeeded, so what the model sees again is the conversation that worked. `ahi tui --no-history` records nothing; `/clear` starts a new session at the next turn.

- `ahi session list [--json]` reads each file line by line (never whole; `--json` rows carry `bytes`) and prints one line per session, newest first: id, start time, gateway and model, working directory, turn count, and the last turn's status and run id. Files this ahi cannot read are named on stderr and skipped.
During generation, the UI receives text deltas immediately. The recorder appends
UTF-8 `text_checkpoint` entries in batches (8 KiB or about half a second), not
one entry per token. Tool calls and turn closure still finalize a single assistant
message for replay. Checkpoints count toward the session size budget but are not
replayed as messages. If the process dies mid-stream, `ahi session show` and the
resumed TUI show the latest checkpointed prefix as unfinished, outside model
history. This is process-interruption recovery, not a power-loss guarantee or a
promise to recover text the provider never delivered.

Quitting during a turn (Ctrl+C or Ctrl+D on a blank draft, or `/quit`) requests
cancellation. Standalone chat flushes the partial answer; a service-backed chat
waits a bounded time for the turn owner to confirm closure and reports an error
if it cannot. Commands record a `command_started` entry after successful spawn;
the tool result carries exit status and bounded output. Terminal redraws and
spinner ticks are not recorded. Re-run the focused PTY check with
`uv run --no-project scripts/smoke-model-picker.py --stream-recovery-only` after
`cargo build`.

- `ahi session show <id|path> [--json] [--branch <seq>]` prints the active path of a session in conversation order: operator lines, replies, the name and target of each tool call, the first line of each tool result, each skill the model read (`— skill <name> (<root>, <size>)`), and the turn lines. An entry type written by a newer ahi is kept in its place and shown as unknown, never treated as corruption. `--branch` follows another branch (a failed attempt) back to the root; `--json` prints the header and those entries as lines. A path shows a session file that is not in the store.
- In a terminal that supports the [Program Resume Protocol (OSC 7577)](https://www.superlogical.com/rex/docs/build/program-resume), a recorded local chat reports how to reopen its saved session. The terminal decides whether to restart it. Reopening does not submit a prompt or repeat tool calls. ahi registers after it has a session ID, updates the record when that ID changes, and clears it on normal exit. The report includes the working directory, local service socket or standalone mode, explicit tool policy, and every resolved storage root (`--config-dir`, `--data-dir`, `--state-root`, and `--cache-dir`). These category-specific flags override `--state-dir` and environment settings; `--state-dir` retains its existing single-root meaning. Restoring therefore uses the same storage paths even if the new shell changes its XDG or AHI directory overrides. The terminal resolves `ahi` on its restored `PATH`; normal startup trust and policy rules apply, not transient mode selections. Bridged (`--via`) sessions and paths that are not UTF-8 or contain control characters are not registered. Without a valid terminal token no report is sent. The token is removed from the process environment before tools or child processes start.
- `ahi --session <id>` opens the terminal chat over a stored ahi session with its recorded model and thinking level and the active path's messages, and appends new turns to it. `ahi tui --session <id>` and `ahi tui --resume <id>` are equivalent entry points. Use the `tui` form for additional chat options such as `--standalone`, `--socket`, or `--via`. The top-level shortcut accepts global options such as `--state-dir` but cannot be combined with a subcommand. As with `ahi tui`, piped output lists providers instead of opening an interactive chat. Quitting a recorded chat with `/quit` or **Ctrl+D** prints its session ID and `ahi --session <id>` after restoring the terminal. Chats without a saved session do not print a resume command.
- `ahi session export <id> --to <file>` copies the session file; the destination must not exist. `ahi session import <file>` copies a session file into the store, or extends the stored copy when the file carries new entries after the ones already stored, and refuses a file whose history differs. Across machines that is the whole protocol: the file moves by any means (`scp`, `rsync`, a multiplexer's file transfer) and `import` reads it.

Prompts, replies, tool arguments (including text written to files), tool output, and the provider's native replay items are all in the file, as they were all sent to the provider. One process writes a session at a time, the chat that holds it; readers skip a last line that is still being written.

### ahi exec

One model turn and exit. `ahi exec --model local/vendor/model -p "summarize the diff"` prints the answer; `git diff | ahi exec --model … ` reads the prompt from stdin; `--prompt-file <path>` reads it from a file (UTF-8, at most 1 MiB). Tools are confined to `--workdir` (default: the current directory) and governed by its `ahi.tools.yaml`, or by `--tools <file>`, which must lie outside the working directory. An `ask` rule prompts on the terminal when a person is attending; `--headless` means no person: the prompt comes only from `--prompt-file`, every `ask` is denied with a reason the model sees, and `--result-file <path>` is required. The result file must not exist beforehand; it receives one JSON document (`status`, `text`, `error`, `usage`, `tools` counts, `effects` of `none`, `some`, or `unknown`, and a `ledger` of every write and command with the same facts as `audit.log`), written owner-only. `--prompt-file`, `--result-file`, and `--tools` are operator files and are refused inside the working directory, where the model's tools could read or overwrite them.

stdout carries the answer, or the whole document with `--format json`; stderr carries diagnostics and, with `--headless` or `-v`, one progress line per tool call (name, target, exit code; never arguments or output). Exit codes: 0 ok, 1 the turn failed, 2 usage, 3 configuration (`models.json`, gateway, model), 4 credential, 5 policy file, 6 cancelled or timed out, 7 session (unknown id, a `--run-id` reused for another prompt or skill selection), 8 the result file could not be written. `--timeout <secs>` (default 600) bounds the whole turn, including a running command; SIGINT, SIGTERM, and SIGHUP cancel it and the document says `cancelled` with the ledger so far. `--run-id <id>` makes the turn idempotent per session: a repeat with the same prompt and skill selections is answered from the record without a model call, and `--prompt-sha256 <hex>` refuses a prompt file that arrived changed. `--session <id>` continues a recorded session (see `ahi session`); every turn is recorded. `--check` validates the arguments, files, model, and credential and exits without a turn. `--thinking <level>` sets the reasoning effort for the turn.

On a host where `ahi serve` is running, the turn goes through it (`--socket <path>` names another socket and fails if nothing answers); otherwise the same service runs inside the command (`--standalone` insists on that). `ahi exec` never starts a server.

### ahi serve

`ahi serve` is the session service: one process per user per host that owns the session store and runs every turn, listening on a Unix socket (`$XDG_RUNTIME_DIR/ahi/serve.sock`, else `<state>/run/serve.sock`, mode 600 in a directory of mode 700). It speaks one JSON object per line in each direction, using ahi's own protocol, not hc's terminal-control protocol: requests (`session.create`, `turn.submit`, `session.subscribe`, `ask.answer`, `turn.cancel`, …) get one answer each, and a subscribed connection receives the session's entries, streamed text, approval requests, and turn results as events. There is no network listener, no token, and no TLS: whoever can open the socket is its owner, and across machines ssh carries the bytes. The server never idles out; `ahi serve stop` ends it (open turns are cancelled and given 5 s to close), as do SIGTERM and SIGINT. `ahi serve start` starts one detached when none answers and prints the socket path; `ahi tui` does the same on its own. `-v` logs each request's method on stderr. A second `ahi serve` on a live socket is refused; a socket file nothing answers on is removed as a crash's leftover.

`ahi tui --via "ssh host ahi connect"` and `ahi exec --via …` attach to a server elsewhere through an explicitly chosen command whose stdio carries ahi's protocol. `ahi connect` copies stdin to the socket and the socket to stdout until either side closes, and its exit code says which: 0 when stdin ended, 3 when the server closed first, 1 when nothing answered. `ssh host ahi connect` is therefore a remote connection to that host's service with ssh's authentication and nothing else, not a Rex or hc terminal-control connection. A protocol smoke request can use `printf '{"id":1,"method":"health","params":{}}\n' | ahi connect`. Not available on Windows, where `ahi exec` runs in its own process.

### ahi login

Sign in with ChatGPT using the current direct-token browser flow. Prints the URL and accepts a pasted full callback URL when the browser cannot reach ahi.

**`/login`** in chat or either model screen opens the same OpenAI sign-in view. It shows the authorization URL, tries the browser, and accepts a pasted full callback URL. **Ctrl+Y** copies the complete raw authorization URL, without UI decoration or soft-wrap line breaks; the callback draft stays untouched. This uses ahi's shared clipboard adapters for macOS, native Windows, Linux, and WSL (including Windows Terminal), with OSC 52 for supported terminals/remote sessions. Clipboard work runs off the UI thread. Feedback reports failure or that the URL was sent; terminal copy requests cannot confirm clipboard acceptance. Retry with Ctrl+Y if necessary. **Esc** or **Ctrl+C** cancels and returns to the originating screen; the active model, transcript, and curated settings are retained. Login never refreshes catalogs automatically; use `r` explicitly afterward. The standalone `ahi login` command remains available.

**OpenAI login now uses the current Sign in with ChatGPT direct-token flow**, not the older Codex fixed-client flow. `ahi login` binds `http://127.0.0.1:1455/auth/callback` before opening the browser, requests an issued client for this installation and the `https://api.openai.com/v1` resource with `chatgpt.tokens.use.direct`. When automatic callback is unavailable, paste the full final callback URL, including `state` and `client_id`; a bare code is not sufficient. Old credentials remain untouched until a new login succeeds; explicit refresh asks you to run `ahi login` to migrate. Browser tokens are never forwarded to proxies: a conflicting OpenAI `baseUrl` causes an error. Register proxies as separate gateways.

### ahi dev

Opens a shell in the opinionated development image that [ahi-sh/ahi-images](https://github.com/ahi-sh/ahi-images) publishes as `ghcr.io/ahi-sh/ahi-dev:latest` (Fedora, a non-root `dev` user with sudo, git, ripgrep, fd, bat, jq, tmux, vim, zsh). ahi does not contain a Containerfile; it drives a container runtime, `podman` or `docker`, found on `PATH`. Without a subcommand it pulls the image when it is missing and runs it interactively with the working directory mounted at `/work`. Only the working directory is mounted: the ahi directories, the home directory, and the container socket stay outside, and the image runs as its own `dev` user. `ahi dev` is a reproducible shell, not a security boundary; its properties are the container runtime's.

| Command | Effect |
| --- | --- |
| `ahi dev` | Pull if missing, then open the shell. Exits with the shell's exit code. |
| `ahi dev --flavor rust` | Open a shell in `ghcr.io/ahi-sh/ahi-dev-rust`. The same flag works with the existing verbs, e.g. `ahi dev --flavor python run -- uv run pytest` or `ahi dev --flavor fpga status`. |
| `ahi dev build` | Pull the image. With `--from <dir>`, build `images/dev` from that local `ahi-images` checkout and tag it with the image reference instead. |
| `ahi dev run -- <command>` | Pull if missing, then run the command in the image with argv passed through untouched. Exits with the command's exit code; stdout and stderr pass through, so it pipes. |
| `ahi dev shell` | Open the shell in an image that is already present; never pulls. |
| `ahi dev status` | Print the runtime, the image reference and where it came from, the mount, and the local image id and creation time. |
| `ahi dev clean` | Remove the local image. |

`--flavor NAME` selects `ghcr.io/ahi-sh/ahi-dev-NAME` by convention, without a registry lookup or a built-in list. Use lowercase letters or digits, optionally grouped with hyphens. Normal tag selection still applies (`ahi dev --flavor rust --tag edge`); a valid name does not guarantee an image has been published. Use `--image` instead for arbitrary references or digests. `--flavor` conflicts with `--image` and `--from` (local builds use `images/dev`). Selecting a flavor does not grant device access or change sandbox policy. To make bare `ahi dev` use a variant in a project, keep the full reference in `ahi.dev.yaml`:

```yaml
version: 1
image: ghcr.io/ahi-sh/ahi-dev-rust
```

The image reference is the first of `--image`, `AHI_DEV_IMAGE`, `image` in the working directory's `ahi.dev.yaml`, the first non-comment line of `dev-image` in the config directory (`~/.config/ahi/dev-image`), and the default `ghcr.io/ahi-sh/ahi-dev`. The tag is the first of `--tag`, `tag` in `ahi.dev.yaml`, and `latest`, appended to that name: `ahi dev --tag edge` opens `ghcr.io/ahi-sh/ahi-dev:edge`. A reference that already names a tag or digest is never rewritten: it is used as is, `--tag` against it is an error, and a repository `tag` yields to a tagged `--image` or `AHI_DEV_IMAGE`. `ahi.dev.yaml` (`version: 1` with optional `image`, `tag`, `shell`, and `runtime`) is a preference a repository ships for its contributors; flags and environment outrank it, and it grants nothing. `--rebuild` pulls or builds again even when the image is present. `--shell <cmd>` picks the program to run; the default is the repository's `shell` (`bash` or `zsh` in the ahi-dev images), else the basename of `SHELL` (so a host `/opt/homebrew/bin/zsh` becomes the image's `zsh`), else `/bin/zsh`, the images' login shell. `--runtime <cmd>`, `AHI_DEV_RUNTIME`, or the repository's `runtime` names the runtime as a command on `PATH` or a path; otherwise `podman` is preferred over `docker`. `-t` is passed only when stdin is a terminal, so a piped script runs too. The container hostname is the image's short name (`ahi-dev-rust`) and the mounted directory's name is passed as `AHI_DEV_PROJECT`, which the image's prompt shows as `dev@ahi-dev-rust:<version> <project>`. On Unix the owner of the mounted directory is passed as `AHI_HOST_UID` and `AHI_HOST_GID`; the ahi-dev entrypoint remaps its `dev` user to them so files written to the mount belong to the host user. Image provenance can be checked with `cosign verify` as the ahi-images README describes; ahi itself does not verify signatures before pulling. `ahi doctor` reports which runtime would be used.

### Service-owned scratch delegation in a headless Rex block

`ahi delegate launch` starts **the worker itself** in a new, dedicated session on an existing local Rex server, with no graphical client. The Unix `ahi serve` process admits the task and freezes the exact route and tool rules before asking Rex to start. The worker claims once, runs the model and its tools in private scratch, and returns a result over the same authenticated service connection. No parent transcript or approval grants are copied.

```sh
ahi delegate launch --server unix:///absolute/path/to/rex.sock \
  --model gateway/model-id --prompt-file task.txt --dry-run
# Remove --dry-run to launch. The first assignment ID is also the child ID.
ahi delegate status RUN_ID --wait 30
ahi delegate follow RUN_ID --prompt-file next.txt   # returns a new run ID
ahi delegate cancel RUN_ID                        # cancel this run, not its child
ahi delegate close CHILD_ID                       # explicitly end the live child
```

**Mode and lifetime.** A direct operator launch uses the current workspace’s startup mode (policy, remembered trust, or Plan). `--mode plan` narrows it; the flag cannot grant trust. The child retains deny, path and unattended approval restrictions. Trusted can narrow to a workspace mode, and Full Access can narrow to Trusted. Between named modes the service requires a provable subset of grants with deny rules preserved; incomparable modes are refused. Even an inherited Full Access label cannot unlock absolute paths, shell commands, skills or recursive delegation in the scratch profile. `--follow-up FILE` is repeatable: each prompt sees the child’s prior replies and tool results. The **run** continues through tool rounds and ends when the queue is empty, or earlier on failure, cancellation or the assignment deadline. The **worker** stays at its prompt. Enter a follow-up in Rex, or use `ahi delegate follow` with the last completed run ID. A stale run ID cannot create a sibling. Each follow-up rechecks the current workspace authority and the previous child ceiling; request a narrower mode if needed. Enter `/close` to end the child from its terminal. Admission is bounded to 16 prompts totaling 1 MiB; a turn exceeding 64 tool rounds fails. There is no synthetic “keep going” prompt. The result contains the final answer and `turns_completed`.

**Calling model.** Set `AHI_DELEGATE_REX_SERVER` in the listening service’s environment to opt into the fixed `rex-scratch` profile. An authorized model can call `delegate` with `{"local":{"profile":"rex-scratch","mode":"plan"},"prompt":"Inspect scratch","follow_ups":["Summarize your findings"]}`. Launch returns a task reference instead of waiting. The parent can take another turn while the child runs. The owning parent can later use `{"local":{"profile":"rex-scratch","action":"status","run":"RUN_ID"}}`; use `follow` with a new top-level `prompt`, `cancel` to stop one run, or `close` to end the child. Omit `mode` to inherit. At admission the service checks the still-running parent session and run ID, its frozen permissions, and the origin workspace’s current trust and policy. A temporary parent-mode selection cannot override a revoked workspace grant. The model cannot pick another server, path or parent. Trusted and Full Access can call this profile; a workspace mode needs `allow.local_profiles: [rex-scratch]`. Remote `workers` grants do not authorize it. A model in Plan cannot delegate. The existing remote `delegate({worker,prompt,…})` protocol is unchanged; these local inheritance guarantees do not apply across a remote connector.

**Recovery and retention.** Select a local Unix socket or loopback IP:port on the same host/account. `status` returns the verified Rex session and block IDs, the run result, and `lifecycle` (latest run, live state and transcript path). The durable transcript is a separate ahi session under the child’s private assignment directory, not a branch of the parent conversation. Read that file with `ahi session show /path/to/transcript.jsonl`, even after closing the child. After a crash, inspect the recorded history; nothing silently replays the last run. `status` reads structured service records, never PTY output. Losing a launch/claim/finish reply is not permission to retry. Cancellation is cooperative; closing a terminal client does not cancel the worker. A missing result is unknown, not success. Prompts, private admission records and results remain under `<state>/delegation/assignments/`; the scratch directory is under the service’s temp directory and is named in `assignment.scratch`. Remove them manually only after the worker stops. Neither mode restrictions nor the peer-checked socket isolate other processes running as the same OS account.

For a disposable real-Rex check without a GUI, credentials or paid inference: `python3 scripts/smoke-rex-delegation.py --binary target/debug/ahi` (Unix, installed Rex CLI). It starts a fresh server on a private socket and uses a loopback model fixture.

### ahi sandbox

Detection only in this version; nothing is confined yet. `ahi sandbox check` prints the platform, whether an outer sandbox is already active on the process, the evidence, and the backend `ahi sandbox run` would use, then exits 0. On Linux it reads `Seccomp:` and `NoNewPrivs:` from `/proc/self/status`, the active LSM list for Landlock, `bwrap` on `PATH` with the user-namespace limits, and container markers. A seccomp filter already active on the process (mode 2, as inside OpenShell) is reported as an outer sandbox, and ahi would run a child directly rather than nest a second filter. The backend order is Landlock plus seccomp, then `bwrap`, then none; macOS reports `sandbox-exec` as best effort because Apple treats it as deprecated; Windows reports a job object, which contains the process tree only. The probes read files and `PATH` and need no unsafe code; the definitive OpenShell test, a `seccomp` call returning `EPERM`, arrives with `ahi sandbox run`. `ahi doctor` carries the same one-line summary.

## Glossary

For children, sessions, runs, and turns, see the [delegation glossary](#delegation-glossary).

| Term | Meaning in ahi |
| --- | --- |
| **Primitive** | A capability with a clear owner, such as tools, policy, sessions, or terminal hosting. Usually a Rust module, not a plugin or separate crate. |
| **Agent runtime** | The loop that asks a model for a response, executes authorized tool calls, and continues the turn. Its separation from chat presentation is still planned. |
| **Provider / gateway** | A provider implements a model API; a gateway exposes models through a configured endpoint. Neither owns tool authorization. |
| **Tool** | A compiled, model-callable operation such as `read_file`, `edit_file`, or `run_command`. Its implementation is separate from permission to run it. |
| **Tool policy / approval** | Workspace mode rules authorize capabilities. An approval answers a particular permission request; model text and tool output cannot grant authority. |
| **Skill** | Instructions loaded as tool-result content under policy. A skill supplies information, not new permissions or executable plugins. |
| **Precommit / Git hook** | Precommit runs repository checks. A Git lifecycle hook invokes that runner; it is not the runner itself. |
| **TUI** | Ahi's terminal user interface: chat, model selection, approval prompts, and transcript presentation. |
| **Terminal host** | An external multiplexer server that provides persistent terminals and runs programs in them. `terminal_host` is ahi's interface to Rex (primary) and hc. It does not implement a multiplexer. |
| **Terminal / shell / UI client** | A terminal supplies program input/output; a shell is one program that can run in it. A graphical UI client displays terminals. Ahi does not manage the external client's windows, layout, or focus. |
| **Execution target** | The selected server/core and local or remote execution namespace. Native Windows and WSL are different targets, even on one physical computer. |
| **Terminal handle** | A backend- and target-qualified native terminal ID. It is not an ahi session ID, worker label, or currently focused pane. Local core IDs are not portable to another machine. |
| **Session service / connector** | `ahi serve` owns structured session operations. `ahi connect` carries that protocol over stdio, including through SSH. `hc connect` carries a different protocol. |
| **Program status** | Advisory OSC 7501 reports such as working or blocked. They are neither structured agent replies nor authorization or proof that a process exited. |
| **Durability / ledger** | Stored conversation and execution facts, including tool presentation details. Rendering and authorization remain application responsibilities. |
| **Conversation resume / crash recovery** | Resume restores a saved conversation. Crash recovery would reconcile unfinished effects and permissions; it is not implemented by conversation resume. |
| **Containment / development environment** | Containment restricts what a process can access. `ahi dev` launches a development container; software path checks and terminal multiplexing are not OS sandboxes. |

See the [module plan](docs/MODULE_STRUCTURE.md) for implemented versus proposed ownership.

## Files and directories

ahi keeps its own files in the XDG base directories, each with an `ahi` child:

| Files | Directory | Default |
| --- | --- | --- |
| `models.json`, `padding.toml`, `theme.toml` | `$XDG_CONFIG_HOME/ahi` | `~/.config/ahi` |
| `credentials/openai.json`, `openai-device-id` | `$XDG_DATA_HOME/ahi` | `~/.local/share/ahi` |
| `selection`, `thinking`, `audit.log`, `sessions/`, `run/serve.sock` (without `$XDG_RUNTIME_DIR`) | `$XDG_STATE_HOME/ahi` | `~/.local/state/ahi` |
| `catalogs/`, `objects/sha256/` | `$XDG_CACHE_HOME/ahi` | `~/.cache/ahi` |

An XDG variable counts only when it is an absolute path, as the specification requires. On Windows without XDG variables the categories live under `%LOCALAPPDATA%\ahi\{config,data,state,cache}`. `--state-dir`, or `AHI_STATE_DIR`, puts every category under one directory for portable or sandboxed setups; `--cache-dir`, or `AHI_HOME`, moves the cache alone. Without a home directory, set the XDG variables or `--state-dir`. ahi never reads `~/.pi`, and it does not move files from older locations: `ahi doctor` prints the directories in use. Earlier versions kept everything in `~/.local/share/ahi`; move `models.json` from there to `~/.config/ahi/`, or pass `--state-dir ~/.local/share/ahi` to keep the old layout.

On Unix every `ahi` directory ahi creates is mode `0700`, and every file it writes there (`models.json`, the credential, the device id, `selection`, `thinking`, `audit.log`) is `0600`. Missing XDG bases such as `~/.config` are created with the normal process umask, and a directory that already exists keeps the mode its owner chose. The credential is a file; XDG decides placement, not encryption, so protect the data directory as you would `~/.ssh`. Like ssh, ahi refuses to use a credential that other users can read, whoever loosened it, and tells you to `chmod 600` it or sign in again; `ahi doctor` warns about any ahi directory or file that is group- or world-accessible. A store whose `objects/sha256` directory already exists can be opened read-only.

## Verifying downloads

A published GitHub release for tag `v<version>` with Apple Silicon support carries six assets. `<version>` is the SemVer string without the leading `v` (tag `v0.28.1` uses `0.28.1`).

- `ahi-<version>-linux-x86_64.tar.gz`
- `ahi-<version>-linux-aarch64.tar.gz`
- `ahi-<version>-windows-x86_64.tar.gz`
- `ahi-<version>-sha256sums.txt` (Linux and Windows)
- `ahi-<version>-macos-aarch64.tar.gz`
- `ahi-<version>-macos-sha256sums.txt` (Apple Silicon only)

Each archive contains one `ahi` executable at the archive root (`ahi.exe` on Windows) and no other members. The Linux archives are statically linked and bit-reproducible on their runner; the Windows and macOS archives are built natively and are not promised to be bit-reproducible. Both checksum manifests use `sha256sum` format.

For an organization release, download the archive and its matching manifest, then verify both. For example, on Linux:

```sh
set -e
tag=v0.28.1
repo=ahi-sh/ahi
archive=ahi-${tag#v}-linux-x86_64.tar.gz
sums=ahi-${tag#v}-sha256sums.txt
commit=$(gh api "repos/$repo/commits/$tag" --jq .sha)
for file in "$sums" "$archive"; do
  gh attestation verify "$file" --repo "$repo" --source-ref "refs/tags/$tag" \
    --source-digest "$commit" --signer-workflow "$repo/.github/workflows/release_binary_assets.yml" \
    --predicate-type https://slsa.dev/provenance/v1
done
sha256sum --check --ignore-missing "$sums"
```

`sha256sum --check` uses the filenames recorded in the sums file, so the archive has to sit beside that file; `--ignore-missing` skips the archives you did not download. Verify each archive you use the same way, and replace `v0.28.1` with the organization release that was downloaded.

## Development

MSRV is Rust 1.88. A `justfile` wraps the same commands CI runs.

```sh
cargo test --all-features
cargo build --release
./target/release/ahi --version
```

`ahi --version` and `ahi version` print only the client release (`ahi` followed by the root manifest version and a newline), without contacting a daemon. From a checkout, use `cargo run -- --version` or `cargo run -- version`. `scripts/smoke-version.sh` checks that line against the root manifest.

For CI, select a field to get just its value and a newline on stdout:

| Field | Client (no daemon needed) | Running daemon (never starts one) |
| --- | --- | --- |
| Release, e.g. `0.28.1` | `ahi version release` | `ahi serve version release` |
| Session protocol, e.g. `1` | `ahi version protocol` | `ahi serve version protocol` |

From the checkout, for example: `protocol=$(cargo run --quiet -- version protocol)`. Server queries accept `--socket <path>`. If the server is unreachable or doesn't report the requested field, the command exits nonzero, writes a diagnostic to stderr, and prints nothing on stdout.

Deterministic Python tests live in `tests/python/`. Run them from the repository root with `uv run --no-project --python 3.11 --with-requirements tests/python/requirements.txt python -m pytest -q` (or install that requirements file in a Python 3.11+ venv and run `python -m pytest -q`). The tests need no network or model after dependency installation. PTY checks and opt-in live evaluations remain explicit `scripts/` commands; pytest never launches them by discovery.

For real terminal verification on POSIX, build and run the maintained workflow against that exact executable:

```sh
cargo build --target-dir target/terminal-experience-verification
uv run scripts/smoke-model-picker.py \
  --binary target/terminal-experience-verification/debug/ahi \
  --capture-directory target/terminal-experience-captures
```

It exercises shared command menus, gateway onboarding through a streamed local reply, model curation, editor handoffs, state/conflict preservation, and the existing chat/OpenAI regressions. Screen captures include 80×24 and 60×24 layouts. It uses disposable state and loopback HTTP only—not your credentials, clipboard, editor configuration, or an external model service. OpenAI callback cases need local port 1455 available.

`scripts/install-supply-chain-tools.sh` downloads cargo-deny 0.20.2 and cargo-audit 0.22.2 release binaries and checks their sha256. Those versions can read the current RustSec database. The project itself builds with Rust 1.88. `just ci` runs that installer, then `cargo deny check` and `cargo audit`.

### Toolchain versions

Three toolchain numbers are decoupled on purpose.

`rust-version = "1.88"` in the root `Cargo.toml` is the MSRV floor, the oldest Rust ahi promises to build with. ahi's own code needs 1.88 (`slice::as_chunks`); the highest floor a dependency declares is `ratatui-core` at 1.86.

CI and the release asset workflows build with a pinned recent stable. Each workflow installs it with the runners' preinstalled rustup and no third-party action, and selects it with a top-level `RUSTUP_TOOLCHAIN`. Local development floats to the latest stable through `rust-toolchain.toml` (`channel = "stable"`). `RUSTUP_TOOLCHAIN` outranks `rust-toolchain.toml` in rustup's precedence, which keeps CI pinned while local floats. Bump the pin and the install steps together when adopting a new stable.

Because ordinary CI builds a recent stable, the MSRV is exercised in exactly one place: the publish workflow checks the declared floor with `cargo +1.88.0 check --all-features --locked` immediately before `cargo publish`. Bump the MSRV only when a security-forced dependency floor or ahi's own code requires it, never because a new stable shipped. A bump is one commit: change `rust-version` in `Cargo.toml`, verify the new floor builds, and record the raise in `CHANGELOG.md` with the next version.

## License

MIT, matching the `LICENSE` file already in this repository. Dual-licensing is an open decision and is not applied here.

`crates/ahi-widgets` is a fork of six widgets from [ratatui](https://github.com/ratatui/ratatui) (`ratatui-widgets` 0.3.0), also MIT; its `LICENSE` carries the upstream copyright and its `FORK.md` lists what was copied and changed.
