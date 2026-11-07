# ahi

`ahi` is a Rust library and command-line tool with two jobs. As a hook runner it reads the [pre-commit](https://pre-commit.com/) configuration file (`.pre-commit-config.yaml`) and runs fast, offline checks: it parses that config, filters files the way pre-commit does, and executes checks compiled into the binary. It does not download runtimes, and it does not clone or run someone else's hook repository. As a terminal chat (`ahi tui`) it signs in to OpenAI with the browser, registers OpenAI-compatible gateways from `models.json`, curates their models, streams replies, and lets the model call read-only workspace tools only where an `ahi.tools.yaml` rule approves them.

## Installation

Every platform can install from crates.io with Rust 1.88 or newer from [rustup](https://rustup.rs/):

```sh
cargo install ahi --locked
ahi --version
```

Run the same command again to update. Linux and Windows can skip Cargo and use the prebuilt, attested archives below; macOS cannot yet (due to instability with GitHub Actions MacOS runners, as of early November 2026).

### Linux

Statically linked archives for **x86_64** and **aarch64** are on the [latest release](https://github.com/iancleary/ahi/releases/latest).

1. Download `ahi-<version>-linux-<architecture>.tar.gz` and `ahi-<version>-sha256sums.txt`.
2. [Verify the checksum and the GitHub build-provenance attestation](#verifying-downloads) before extracting.
3. Extract and install `ahi` somewhere on your `PATH`:

   ```sh
   VERSION=0.6.0 ARCH=x86_64   # or aarch64
   tar -xzf "ahi-$VERSION-linux-$ARCH.tar.gz"
   install -D -m 755 ahi "$HOME/.local/bin/ahi"
   ahi --version
   ```

Afterwards `ahi update` replaces the binary with the next release's archive, verifying the checksum file and the archive with the [GitHub CLI (`gh`)](https://cli.github.com/) before touching anything. It does not need Cargo.

### Windows

An **x86_64** archive is on the [latest release](https://github.com/iancleary/ahi/releases/latest).

1. Download `ahi-<version>-windows-x86_64.tar.gz` and `ahi-<version>-sha256sums.txt`.
2. [Verify the checksum and attestation](#verifying-downloads). In PowerShell, `Get-FileHash <archive> -Algorithm SHA256` gives the checksum to compare with the sums file, and `gh attestation verify` checks provenance.
3. Extract in PowerShell and put `ahi.exe` on your user `PATH`:

   ```powershell
   tar -xzf ahi-0.6.0-windows-x86_64.tar.gz
   .\ahi.exe --version
   ```

   The binary is unsigned, so SmartScreen may prompt on first run.

`ahi update` works here too and requires `gh`.

### macOS

Use `cargo install ahi --locked`, above. Prebuilt macOS archives and `ahi update` are deferred while GitHub-hosted ARM64 runners lack capacity; `ahi update` on macOS prints that pointer and makes no network request. See the [restoration checklist](docs/macos-update-follow-up.md).

## Getting started

```sh
ahi doctor          # versions, the config, data, state and cache directories, and any mode warnings
ahi login           # sign in to OpenAI in the browser (optional)
ahi tui             # register a gateway with /gateway, curate models, chat
ahi run --all-files # in a repository with .pre-commit-config.yaml
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
ahi run --fix
```

`examples/tools` holds chat tool policies (`ahi.tools.yaml`) from read-everything to deny-all, for `ahi tui --tools <file>` or copying into a repository; its README describes each.

`examples/starter/pre-commit` is a sample hook:

```sh
#!/bin/sh
exec ahi run --fix
```

A repository can carry up to four ahi-related files, one per job. The two allowlists share one rule grammar (`id`, `action`, `match`, `effect`, optional `ask`), and the filename says which trust boundary a diff touches. Unknown keys are errors in every file, and the retired names `ahi.policy.yaml` and `ahi.actions.yaml` are refused rather than ignored.

| File | Decides | Read by |
| --- | --- | --- |
| `.pre-commit-config.yaml` | Which hooks run; upstream pre-commit format | `ahi run`, `ahi migrate` |
| `ahi.hooks.yaml` | Which local `system`/`script` hooks may execute, by id and exact entry (`action: hook`) | `ahi run` |
| `ahi.tools.yaml` | Which builtin read-only tools (`read_file`, `list_dir`, `find_files`, `search_files`) the model may call, where, and whether to ask (`action: tool_call`) | `ahi tui` |
| `ahi.dev.yaml` | The development image, tag, shell, and runtime this repository prefers; grants nothing | `ahi dev` |

## Commands

Every subcommand also accepts global flags: `-c/--config <path>` selects the config file, `--cache-dir <dir>` overrides `AHI_HOME` and `XDG_CACHE_HOME`, `--state-dir <dir>` overrides `AHI_STATE_DIR` and keeps every ahi directory under one root, and `--color auto|always|never` is validated but this version prints plain text either way.

| Command | Summary |
| --- | --- |
| [`ahi run [HOOK]`](#ahi-run) | Run the configured hooks on staged files, a file list, or a ref range, with text, JSON, or SARIF output. |
| [`ahi list-checks`](#ahi-list-checks) | List the builtin check ids and aliases. |
| [`ahi explain <id>`](#ahi-explain) | Describe one builtin check and the environment names local hooks receive. |
| [`ahi doctor`](#ahi-doctor) | Report versions, paths, directories, modes, and policy. |
| [`ahi migrate`](#ahi-migrate) | Suggest an ahi config from an existing `.pre-commit-config.yaml`. |
| [`ahi hooks install`, `ahi hooks uninstall`](#ahi-hooks) | Hook shim commands; not implemented, exit 1. |
| [`ahi update`](#ahi-update) | Replace the binary with the latest attested release archive. |
| [`ahi tui [--refresh]`](#ahi-tui) | Pick curated models, register gateways, and chat, with policy-gated read-only tools. |
| [`ahi login`](#ahi-login) | Sign in with ChatGPT in the browser. |
| [`ahi dev [build\|run\|shell\|status\|clean]`](#ahi-dev) | Open a shell, or run a command, in the published development image with the working directory mounted. |
| [`ahi sandbox check`](#ahi-sandbox) | Report the containment backend this host offers and whether an outer sandbox is already active. |

### ahi run

Staged files. `[HOOK]` runs only that hook id or alias. `--all-files`, `--files`, and `--from-ref`/`--to-ref` select other sets, `--hook-stage` selects a stage other than pre-commit, and `--commit-msg-filename` feeds the commit-message path to commit-msg and prepare-commit-msg stages. `--format` is `text`, `json`, or `sarif`. `--fail-fast` stops after the first failing hook, and `--verbose`/`-v` shows passing local-hook output. `--fix` rewrites end-of-file, trailing whitespace, and mixed line endings.

Checkers are `repo: builtin`. `ahi lock`, `ahi verify`, `ahi fetch`, `self-update`, `install`, and `uninstall` are not commands. The hook shim commands are `ahi hooks install` and `ahi hooks uninstall`, and they exit 1. `repo: local` with `language: system` or `language: script` runs when an `ahi.hooks.yaml` rule approves that hook id and the exact `entry` string. The entry is split without a shell. A missing rule prints the snippet to add and does not run the command. Other local languages fail the hook and say `not supported by ahi`. `repo: meta` fails with an explicit error. A URL repo fails the hook and is not fetched. Nothing is skipped silently.

ahi runs tools that are already installed. It can pass a toolchain manager the variables it needs to find itself. It does not install, update, resolve, or cache a toolchain or dependencies for a local hook.

| | Names |
| --- | --- |
| Passed by default, if set | `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, plus `PATH`, `HOME`, and the other base names in the spec. `PRE_COMMIT=1` is set. |
| Policy only (`toolchain_env`) | `GOROOT`, `GOPATH`, `VIRTUAL_ENV`, `NVM_DIR`. Not enabled by default. `NVM_DIR` does not enable Node and does not run `npm install`. |
| Never passed | Names containing `TOKEN`, `SECRET`, `PASSWORD`, or `KEY`, and names starting with `AWS_`, `AZURE_`, or `GITHUB_`. Listing one in the policy is an error. |

`ahi explain` prints those names. The audit line records the names passed to each local hook and does not record values. A global `toolchain_env` list applies to every local hook. A `toolchain_env` list on one rule applies only to that rule's id and entry.

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

### ahi list-checks

Builtin check ids with aliases. `--format` is `text` (default) or `json`.

The ids, aliases, and what each check reports are the same table `ahi explain` draws on. The hook configuration example under [ahi run](#ahi-run) lists every builtin id.

### ahi explain

What one check reports.

It also prints the environment names a local hook receives by default; see the table under [ahi run](#ahi-run).

### ahi doctor

Version, git, config path, the `models.json`, data, state, and cache directories (and whether the cache is writable), scratch directory, policy summary. Sandbox detection is not in v0.1.

It warns about any ahi directory or file that other users can read; see [Files and directories](#files-and-directories).

### ahi migrate

Best-effort suggestion from `.pre-commit-config.yaml`. Prints a report in four parts, "Builtin equivalents", "Local hooks kept as repo: local", "Not ported" with a reason per hook, and the `ahi.hooks.yaml` rules the local hooks need, then the suggested config and a closing line that says the file was left unchanged. Does not guarantee feature parity or the same file scope. Does not write unless `--write` is set, and does not replace an existing file unless `--force` is also set. `ahi run` does not use the mapping.

`ahi migrate` is a reading aid, not upstream `migrate-config`. It does not rewrite `sha`, `python_venv`, or legacy stage names, and it does not fetch a `rev`. Hooks with an obvious builtin id are suggested as `repo: builtin`. `repo: local` system and script hooks are copied with the policy snippet they need. Everything else is listed under "Not ported" with a reason. A run-time substitution of remote hook ids was removed; native checks run only from `repo: builtin`.

### ahi hooks

Exit 1. They are the hook shim commands and are not implemented. Top-level `install` and `uninstall` are not commands.

### ahi update

Replaces the running binary with the latest release from the attested archives. Verifies the sha256sums file and the target archive against their GitHub build-provenance attestations with the `gh` CLI, checks the archive sha256, extracts the single `ahi` member, and atomically renames it over the running binary (moving the running executable aside first on Windows). Refuses any target it cannot verify. Covers linux-x86_64, linux-aarch64, and windows-x86_64; macOS and other targets refuse with a `cargo install ahi` pointer. Does not accept a package name and is not named `self-update`.

Installation and the manual verification steps are under [Installation](#installation) and [Verifying downloads](#verifying-downloads).

### ahi tui

Pick curated models from `models.json`. Load is local-only; `--refresh` explicitly refreshes reference catalogs. Enter saves a selection and opens chat; configured OpenAI-compatible gateways stream real model responses. A pipe prints the load and catalog membership.

#### Picker and curation

In 0.3.0, model catalogs and curated selections are separate. The main picker offers **only** `providers.<id>.models` from `models.json`, in saved order, for OpenAI and every gateway. An omitted list stays empty even after refresh. Reference catalogs never supply picker defaults; their curation view lets you explicitly choose the saved list. See [CHANGELOG.md](CHANGELOG.md) for the breaking gateway-loading change.

The curated picker includes OpenAI and every registered gateway. Provider groups follow the key order of the `providers` object in `models.json`. An implicit, unconfigured OpenAI group starts first; an explicit gateway reorder saves its position too. Highlight a **provider/gateway header** and use **Shift+Up/Down** or **Shift+J/K** to move the whole group and save its order, without changing its models or credentials.

Press **`/`** in **Chat, Models, or Catalog** to open the same **Commands** popup. **Add gateway** (`/gateway`) is the first action, followed by model choice, catalog browsing, OpenAI sign-in, and Quit. New installations show this onboarding path explicitly. Gateway setup guides you through **Connect → Review → Choose models**; registration results and cache warnings appear in the destination catalog immediately. Type `/login` for ChatGPT sign-in without configuring a model first. See [Gateway discovery](docs/GATEWAY_DISCOVERY.md) for the URL contract and [the UI audit](docs/TERMINAL_EXPERIENCE.md) for the design decisions. `r` refreshes catalogs and `c` opens a provider's curation view. Checked models appear first in saved order, followed by unchecked catalog IDs. **Up/Down or j/k** moves the highlight. **Shift+Up/Down or Shift+j/k** reorders the highlighted checked model and saves. **Space** checks/unchecks and immediately saves. Configured IDs missing from the catalog remain checked and removable. **Enter** on a checked catalog model saves the active selection and opens/resumes chat; an unchecked model first needs **Space**. `y` copies an ID (or prints it on exit if no clipboard tool succeeds), and Esc returns to Models with the relevant provider and checked model still highlighted. Each curated ID shows `listed`, `absent from catalog`, or `catalog unavailable`.

#### Chat

**Enter** on a curated model saves the selection and opens chat. A configured gateway is called at `<baseUrl>` plus the path its `api` implies: `/chat/completions` for `openai-completions`, `/responses` for `openai-responses`, `/messages` for `anthropic-messages`, streaming the conversation history. This sends your messages to the selected provider and may incur charges. `apiKey` and custom header values resolve the marker `gateway` or `$ENV_NAME` at submission time; secrets are not stored in `models.json`. Curated OpenAI models use browser-login credentials with `POST https://api.openai.com/v1/responses`. Setting `ANTHROPIC_API_KEY` adds a built-in `anthropic` provider that talks to `https://api.anthropic.com/v1/messages` with that key. See [Providers](docs/PROVIDERS.md) for the protocol, authentication, and catalog matrix, and [OpenAI inference](docs/OPENAI_INFERENCE.md) for the browser-login transport. Every protocol validates streamed function calls and runs the policy-gated builtin tools described below through one turn loop. Model output is not execution authority. [Tool calls](docs/TOOL_CALLS.md) distinguishes this implemented subset from the future general agent runtime. Invalid/incomplete calls fail closed; tool arguments are not printed in notices or parser errors.

#### Tools and policy

Tools are off unless the working directory has an `ahi.tools.yaml` rule that approves one, for every provider. Only builtin, read-only tools exist: `read_file` (whole or a window of lines, with a continuation offset when the 64 KiB cap is hit), `list_dir`, `find_files` (glob over names or paths, honoring `.gitignore`), and `search_files` (regular expression or literal, `path:line:text`). They are compiled into ahi, run in process with no `rg`, `fd`, or `git` subprocess, use software path checks within the working directory, exclude ahi's own directories, and return sorted, bounded output. These in-process checks are not an OS sandbox. A rule with `ask: true` shows an approval prompt in chat that lists the answers for each call: move the highlight with **↑/↓** and apply it with **Enter**, jump to another numbered call with **Shift+↑/↓**, or press the answer's letter (**y** once, **s** for this session under the shown directory and destination, **n** deny, **a** approve the rest, **Esc** deny the rest). Dismissing the prompt cancels the whole batch, including calls a rule had approved. Model output and tool results never grant authority. Without the file, no `tools` are sent and requested calls fail with `tool execution is disabled`. `ahi tui --tools <file>` uses that policy file instead of the working directory's `ahi.tools.yaml`; it must exist and parse, and its rule paths still apply to the working directory. `examples/tools` holds ready-made policies for trying the tools. Approved file contents are sent to the selected provider. Invalid/incomplete calls fail closed; raw tool arguments are not printed in notices or errors. See [Chat screen](docs/CHAT_SCREEN.md#tool-calls).

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
```

#### Requests and limits

Token usage comes only from reported responses. Known catalog context capacity feeds the context meter; complete price/usage information enables cumulative reported-usage cost in Commands, not a claim about the provider's billing total. Reasoning effort is sent only when model metadata explicitly permits it and thinking is not Off. Gateways with a declared `maxTokens` receive a conservative output budget of `min(4096, maxTokens)` through the declared token field; browser-token Responses requests omit unsupported output-limit fields. Exact serialized request-byte limits are enforced before submission. See [Gateway model configuration](docs/GATEWAY_MODEL_CONFIGURATION.md) for supported fields, precedence, and limits. Only one turn runs at a time; Enter preserves the next draft while a reply is active.

Requests refuse redirects, have a 120-second timeout, and cap events at 1 MiB and the total stream at 16 MiB. A gateway that cannot be reached (DNS or connection failure) gets three connection attempts with short pauses; generation is never automatically retried, and after a connection failure **Enter** on a blank input resends the same message. OpenAI can renew once and resubmit after a definitive pre-stream HTTP 401; ambiguous failures or failures after content never retry generation. **Esc** cancels a streaming response: the partial reply is kept and sent with the next message, and the connection is closed at the next read. Exiting chat does the same without the notice. Neither can make a provider stop generating before it notices the closed socket.

#### Commands popup

The shared **Commands** popup opens immediately when you type **`/`**. Keep typing to filter by command, alias, or action label; use **Up/Down** to select, **Enter** to apply, and **Esc** to dismiss without executing. Chat retains its draft on dismissal; Models/Catalog return to list navigation. Single-line command paste in Models/Catalog is bounded and never submits by itself. Chat additionally offers thinking levels, mouse mode, and conversation clearing. Commands are local actions, never model prompts, and exact typed commands continue to work. During an active response, Enter preserves a non-quit command draft; press Enter again after the response finishes to apply it. **`$`** remains reserved for future skill invocation and does not open Commands.

**`/models`** (alias **`/model`**) reopens the curated picker across all providers with locally reloaded settings; selecting a model keeps the transcript, and cancelling keeps the active model. **`/catalog`** opens the active provider's full cached catalog for curation, including uncurated models; `r` explicitly refreshes it, Esc returns to the all-provider picker, and `q` returns to chat. No catalog is fetched just by opening it. **`/quit`** (alias **`/exit`**) closes ahi, even during an active response; upstream cancellation is not guaranteed. `/quit` also exits from a nested Models or Catalog screen; it is different from `q`, which cancels the picker and returns to chat when one exists. At the initial picker, `q` exits even if an old selection exists. **Ctrl+D** on empty chat input exits.

#### Clipboard and selection

Chat has mouse capture **on by default**, with wheel scrolling, clickable jump controls, and app-managed text selection. **Left-drag transcript text** to select it with a background highlight; releasing the drag copies it to the clipboard. **Ctrl+Y** copies the retained selection again; **Esc** clears the highlight without clearing your draft, and with no highlight it cancels a streaming response. Copy feedback appears in the input rule, never in the model transcript. On native Windows, ahi writes the system clipboard through the Win32 API, falling back to `powershell.exe` reading a private UTF-8 temporary file if the clipboard stays busy. On Linux/WSL, ahi tries display-appropriate clipboard tools first (`wl-copy` for Wayland, `xclip`/`xsel` for X11). If those fail on WSL, Windows Terminal receives a bounded OSC 52 copy request; other WSL terminals fall back to `wslpath` and `powershell.exe` using a private UTF-8 temporary file. Headless Linux and SSH/mosh sessions also use OSC 52; set `AHI_CLIPBOARD_OSC52=1` to request it for remote clients without SSH markers. OSC 52 requires terminal support and cannot acknowledge acceptance; its base64 payload is limited to 100,000 characters. If no clipboard adapter succeeds, the selection stays highlighted for retry. **Ctrl+S** (or **`/select`**) enters a keyboard selection mode over the transcript: **h/j/k/l** or the arrows move a cursor, **w**/**b** move by word using Vim's character classes (keyword, punctuation, emoji, and one class per CJK script, so `foo.bar` is three words and a run of ideographs is one), **0**/**$** jump within the row, **g**/**G** jump to the oldest/newest row, Page Up/Down move by a page, **v** starts a character-wise visual range, **V** a line-wise one over whole logical lines with soft wraps rejoined and a trailing newline on yank, exactly as in Vim, **y** yanks it to the clipboard and leaves the mode with the highlight kept for a Ctrl+Y retry, and **Esc** (or Ctrl+C) leaves without copying. The transcript scrolls to keep the cursor visible, selection keys never reach the draft, and the mode works with mouse capture on or off. Native terminal **Cmd+C** is not the app's copy shortcut; use **`/mouse off`** if you prefer terminal-native drag selection and copying. You can also scroll the chat transcript with **Page Up / Page Down**. **Ctrl+K** (or **Ctrl+Home**) jumps to the top; **Ctrl+L** (or **Ctrl+End**) jumps to the bottom and resumes following streamed output.

Use **`/mouse on`** to re-enable application mouse handling: the wheel scrolls the transcript, and floating **Top**/**Bottom** controls support hover and click. Wheel scrolling preserves selected transcript rows, and click-without-drag still activates the jump controls. Copying rejoins soft-wrapped rows while preserving original newlines and spaces, and handles wide Unicode characters and combining marks. Resizing the transcript width, clearing chat, or changing mouse mode clears selection. Dragging onto the top row or below the transcript auto-scrolls one row per frame while the button is held, extending the highlight from the original anchor into newly revealed text; moving back inside the viewport or releasing stops it. The wheel still extends a selection too. **`/mouse off`** restores native selection and hides those controls; **`/mouse`** reports the current mode. The mode is session-local and starts on each time you launch chat. Scrolling up keeps your reading position fixed as new output arrives. Plain Home/End still move within the input, and Ctrl+J still inserts a newline.

#### Editing models.json

A starter `models.json` is in [Getting started](#getting-started). Explicit checkbox/reorder saves atomically replace `models.json` in the config directory, pretty-printing JSON while preserving other providers and settings. Gateway reordering intentionally changes provider key order; model curation preserves it. Stale external edits or save failures leave the file and loaded list unchanged; press `e` to review/reload before retrying. Merely browsing does not create a file; an explicit checkbox action can.

`e` opens `models.json` in `$VISUAL` or `$EDITOR` (notepad fallback on Windows). Other systems ask for an editor when neither is set. Editor-only reload never rewrites the editor's bytes and never fetches. Invalid edits retain the previous registry and display the file path and line.

#### Catalogs and refresh

Explicit OpenAI refresh attempts `GET https://api.openai.com/v1/models` only with new direct-token credentials. **A live request with a new direct-token credential returned HTTP 200 and model metadata.** That response used `models[].slug`; ahi accepts this shape as well as the standard `data[].id` format, preserving every reported ID in order. The earlier HTTP 403 was from an old Codex-flow credential, not the new grant. A 403 preserves the catalog and curated IDs and reports unavailable discovery. Expiry is checked before requests; authentication failures permit at most one renewal and retry. Requests refuse redirects, have a 15-second timeout and a 1 MiB response cap. Pi uses this grant for Responses, not Decisions; ahi now implements the corresponding Responses transport. Configured gateway inference uses a separate Chat Completions backend. Local transport/authentication fixtures passed, but no live OpenAI inference was performed for this implementation. `auth signed-in` means only a credential file exists.

Gateway catalog refresh requests `GET <origin>/agent/models.json`, stripping the path from the configured `baseUrl`. Both the original flat catalog and supported Pi-shaped `{"providers": {"home": {...}}}` documents are accepted. A validated returned same-origin `baseUrl` supplies the inference path for new registration and is shown in Review; no `/v1` is guessed. Multiple candidates require an exact inference URL to disambiguate. Metadata survives caching and curation; discovery never automatically selects remote models. This is a gateway-specific discovery convention, not an OpenAI or industry-standard catalog endpoint. The gateway retains `/pi/agent/model.json` as a legacy alias; ahi uses only the canonical route. Gateways without this catalog route can still serve inference for explicitly configured model IDs.

`ahi tui` never fetches on startup. `ahi tui --refresh` explicitly requests refreshes, preserves old catalogs on failure, and in a pipe exits 1 if any refresh failed while still printing the local load. Catalogs are atomically replaced objects containing provider, source URL, retrieval timestamp, ordered reported IDs, and typed metadata by model ID; old array-format and identifier-only object catalogs remain readable with unknown metadata. `retrieval time unknown (old catalog format)` means that the cached array lacks a timestamp, not that authentication is old. Refresh never edits the curated file.

### ahi login

Sign in with ChatGPT using the current direct-token browser flow. Prints the URL and accepts a pasted full callback URL when the browser cannot reach ahi.

**`/login`** in chat or either model screen opens the same OpenAI sign-in view. It shows the authorization URL, tries the browser, and accepts a pasted full callback URL. **Esc** or **Ctrl+C** cancels and returns to the originating screen; the active model, transcript, and curated settings are retained. Login never refreshes catalogs automatically; use `r` explicitly afterward. The standalone `ahi login` command remains available. See [OpenAI login: flow, limits, and verification](docs/OPENAI_LOGIN.md).

**OpenAI login now uses the current Sign in with ChatGPT direct-token flow**, not the older Codex fixed-client flow. `ahi login` binds `http://127.0.0.1:1455/auth/callback` before opening the browser, requests an issued client for this installation and the `https://api.openai.com/v1` resource with `chatgpt.tokens.use.direct`. When automatic callback is unavailable, paste the full final callback URL, including `state` and `client_id`; a bare code is not sufficient. Old credentials remain untouched until a new login succeeds; explicit refresh asks you to run `ahi login` to migrate. Browser tokens are never forwarded to proxies: a conflicting OpenAI `baseUrl` causes an error. Register proxies as separate gateways.

### ahi dev

Opens a shell in the opinionated development image that [iancleary/ahi-images](https://github.com/iancleary/ahi-images) publishes as `ghcr.io/iancleary/ahi-dev:latest` (Fedora, a non-root `dev` user with sudo, git, ripgrep, fd, bat, jq, tmux, vim, zsh). ahi does not contain a Containerfile; it drives a container runtime, `podman` or `docker`, found on `PATH`. Without a subcommand it pulls the image when it is missing and runs it interactively with the working directory mounted at `/work`. Only the working directory is mounted: the ahi directories, the home directory, and the container socket stay outside, and the image runs as its own `dev` user. `ahi dev` is a reproducible shell, not a security boundary; its properties are the container runtime's. [Development container and sandbox](docs/DEV_CONTAINER.md) explains how it relates to the planned `ahi sandbox`.

| Command | Effect |
| --- | --- |
| `ahi dev` | Pull if missing, then open the shell. Exits with the shell's exit code. |
| `ahi dev build` | Pull the image. With `--from <dir>`, build `images/dev` from that local `ahi-images` checkout and tag it with the image reference instead. |
| `ahi dev run -- <command>` | Pull if missing, then run the command in the image with argv passed through untouched. Exits with the command's exit code; stdout and stderr pass through, so it pipes. |
| `ahi dev shell` | Open the shell in an image that is already present; never pulls. |
| `ahi dev status` | Print the runtime, the image reference and where it came from, the mount, and the local image id and creation time. |
| `ahi dev clean` | Remove the local image. |

The image reference is the first of `--image`, `AHI_DEV_IMAGE`, `image` in the working directory's `ahi.dev.yaml`, the first non-comment line of `dev-image` in the config directory (`~/.config/ahi/dev-image`), and the default `ghcr.io/iancleary/ahi-dev`. The tag is the first of `--tag`, `tag` in `ahi.dev.yaml`, and `latest`, appended to that name: `ahi dev --tag edge` opens `ghcr.io/iancleary/ahi-dev:edge`. A reference that already names a tag or digest is never rewritten: it is used as is, `--tag` against it is an error, and a repository `tag` yields to a tagged `--image` or `AHI_DEV_IMAGE`. `ahi.dev.yaml` (`version: 1` with optional `image`, `tag`, `shell`, and `runtime`) is a preference a repository ships for its contributors; flags and environment outrank it, and it grants nothing. `--rebuild` pulls or builds again even when the image is present. `--shell <cmd>` picks the program to run; the default is the repository's `shell` (`bash` or `zsh` in the ahi-dev images), else the basename of `SHELL` (so a host `/opt/homebrew/bin/zsh` becomes the image's `zsh`), else `/bin/zsh`, the images' login shell. `--runtime <cmd>`, `AHI_DEV_RUNTIME`, or the repository's `runtime` names the runtime as a command on `PATH` or a path; otherwise `podman` is preferred over `docker`. `-t` is passed only when stdin is a terminal, so a piped script runs too. The container hostname is the image's short name (`ahi-dev-rust`) and the mounted directory's name is passed as `AHI_DEV_PROJECT`, which the image's prompt shows as `dev@ahi-dev-rust:<version> <project>`. On Unix the owner of the mounted directory is passed as `AHI_HOST_UID` and `AHI_HOST_GID`; the ahi-dev entrypoint remaps its `dev` user to them so files written to the mount belong to the host user. Image provenance can be checked with `cosign verify` as the ahi-images README describes; ahi itself does not verify signatures before pulling. `ahi doctor` reports which runtime would be used.

### ahi sandbox

Detection only in this version; nothing is confined yet. `ahi sandbox check` prints the platform, whether an outer sandbox is already active on the process, the evidence, and the backend `ahi sandbox run` would use, then exits 0. On Linux it reads `Seccomp:` and `NoNewPrivs:` from `/proc/self/status`, the active LSM list for Landlock, `bwrap` on `PATH` with the user-namespace limits, and container markers. A seccomp filter already active on the process (mode 2, as inside OpenShell) is reported as an outer sandbox, and ahi would run a child directly rather than nest a second filter. The backend order is Landlock plus seccomp, then `bwrap`, then none; macOS reports `sandbox-exec` as best effort because Apple treats it as deprecated; Windows reports a job object, which contains the process tree only. The probes read files and `PATH` and need no unsafe code; the definitive OpenShell test, a `seccomp` call returning `EPERM`, arrives with `ahi sandbox run`. `ahi doctor` carries the same one-line summary. [Development container and sandbox](docs/DEV_CONTAINER.md) has the plan.

## Files and directories

ahi keeps its own files in the XDG base directories, each with an `ahi` child:

| Files | Directory | Default |
| --- | --- | --- |
| `models.json` | `$XDG_CONFIG_HOME/ahi` | `~/.config/ahi` |
| `credentials/openai.json`, `openai-device-id` | `$XDG_DATA_HOME/ahi` | `~/.local/share/ahi` |
| `selection`, `thinking`, `audit.log` | `$XDG_STATE_HOME/ahi` | `~/.local/state/ahi` |
| `catalogs/`, `objects/sha256/` | `$XDG_CACHE_HOME/ahi` | `~/.cache/ahi` |

An XDG variable counts only when it is an absolute path, as the specification requires. On Windows without XDG variables the categories live under `%LOCALAPPDATA%\ahi\{config,data,state,cache}`. `--state-dir`, or `AHI_STATE_DIR`, puts every category under one directory for portable or sandboxed setups; `--cache-dir`, or `AHI_HOME`, moves the cache alone. Without a home directory, set the XDG variables or `--state-dir`. ahi never reads `~/.pi`, and it does not move files from older locations: `ahi doctor` prints the directories in use. Earlier versions kept everything in `~/.local/share/ahi`; move `models.json` from there to `~/.config/ahi/`, or pass `--state-dir ~/.local/share/ahi` to keep the old layout.

On Unix every `ahi` directory ahi creates is mode `0700`, and every file it writes there (`models.json`, the credential, the device id, `selection`, `thinking`, `audit.log`) is `0600`. Missing XDG bases such as `~/.config` are created with the normal process umask, and a directory that already exists keeps the mode its owner chose. The credential is a file; XDG decides placement, not encryption, so protect the data directory as you would `~/.ssh`. Like ssh, ahi refuses to use a credential that other users can read, whoever loosened it, and tells you to `chmod 600` it or sign in again; `ahi doctor` warns about any ahi directory or file that is group- or world-accessible. A store whose `objects/sha256` directory already exists can be opened read-only.

## Verifying downloads

A published GitHub release for tag `v<version>` carries four assets. `<version>` is the SemVer string without the leading `v` (tag `v0.6.0` uses `0.6.0`).

- `ahi-<version>-linux-x86_64.tar.gz`
- `ahi-<version>-linux-aarch64.tar.gz`
- `ahi-<version>-windows-x86_64.tar.gz`
- `ahi-<version>-sha256sums.txt`

Each archive contains one `ahi` executable at the archive root (`ahi.exe` on Windows) and no other members. The Linux archives are statically linked and bit-reproducible on their runner; the Windows archive is built natively and is not promised to be bit-reproducible. On macOS use `cargo install ahi` until [native updates are restored](docs/macos-update-follow-up.md). The sums file is `sha256sum` output for those three archives in the order listed above.

From the directory that contains the downloaded files, for release `0.6.0`:

```sh
sha256sum --check --ignore-missing ahi-0.6.0-sha256sums.txt
gh attestation verify ahi-0.6.0-linux-x86_64.tar.gz --repo iancleary/ahi
gh attestation verify ahi-0.6.0-sha256sums.txt --repo iancleary/ahi
```

`sha256sum --check` uses the filenames recorded in the sums file, so the archive has to sit beside that file; `--ignore-missing` skips the archives you did not download. Verify each archive you use the same way, and replace `0.6.0` with the release that was downloaded. Backfilling assets onto an older release is described in [docs/release.md](docs/release.md).

Archive layout, runners, and the reproducibility flags are in [docs/release.md](docs/release.md).

## Development

MSRV is Rust 1.88. A `justfile` wraps the same commands CI runs.

```sh
cargo test --all-features
cargo build --release
./target/release/ahi --version
```

`ahi --version` prints `ahi` followed by the root manifest version and a newline. `scripts/smoke-version.sh` checks that line against the root manifest.

For real terminal verification on POSIX, build and run the maintained workflow against that exact executable:

```sh
cargo build --target-dir target/terminal-experience-verification
uv run scripts/smoke-model-picker.py \
  --binary target/terminal-experience-verification/debug/ahi \
  --capture-directory target/terminal-experience-captures
```

It exercises shared command menus, gateway onboarding through a streamed local reply, model curation, editor handoffs, state/conflict preservation, and the existing chat/OpenAI regressions. Screen captures include 80×24 and 60×24 layouts. It uses disposable state and loopback HTTP only—not your credentials, clipboard, editor configuration, or an external model service. OpenAI callback cases need local port 1455 available. See [the terminal experience audit and verification guide](docs/TERMINAL_EXPERIENCE.md).

`scripts/install-supply-chain-tools.sh` downloads cargo-deny 0.20.2 and cargo-audit 0.22.2 release binaries and checks their sha256. Those versions can read the current RustSec database. The project itself builds with Rust 1.88. `just ci` runs that installer, then `cargo deny check` and `cargo audit`.

### Toolchain versions

Three toolchain numbers are decoupled on purpose.

`rust-version = "1.88"` in the root `Cargo.toml` is the MSRV floor, the oldest Rust ahi promises to build with. ahi's own code needs only 1.83. The floor is set by the dependency tree: `ratatui-core` 0.1.2 and every security-patched `time` release (0.3.47 and later, the RUSTSEC-2026-0009 fix) require 1.88, and `cargo deny check` and `cargo audit` refuse the vulnerable `time` releases, so no allowed dependency set has a lower floor.

CI and the release asset workflows build with a pinned recent stable. Each workflow installs it with the runners' preinstalled rustup and no third-party action, and selects it with a top-level `RUSTUP_TOOLCHAIN`. Local development floats to the latest stable through `rust-toolchain.toml` (`channel = "stable"`). `RUSTUP_TOOLCHAIN` outranks `rust-toolchain.toml` in rustup's precedence, which keeps CI pinned while local floats. Bump the pin and the install steps together when adopting a new stable.

Because ordinary CI builds a recent stable, the MSRV is exercised in exactly one place: the publish workflow checks the declared floor with `cargo +1.88.0 check --all-features --locked` immediately before `cargo publish`. Bump the MSRV only when a security-forced dependency floor or ahi's own code requires it, never because a new stable shipped. A bump is one commit: change `rust-version` in `Cargo.toml`, verify the new floor builds, extend the chain in `docs/SPEC.md`, and record the raise in `CHANGELOG.md` with the next version.

## License

MIT, matching the `LICENSE` file already in this repository. Dual-licensing is an open decision and is not applied here.