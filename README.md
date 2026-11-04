# ahi

`ahi` is a Rust library and command-line tool that reads the [pre-commit](https://pre-commit.com/) configuration files (`.pre-commit-config.yaml` and `.pre-commit-hooks.yaml`) and runs fast, offline checks. It parses that config, filters files the way pre-commit does, and executes checks compiled into the binary. It does not download runtimes, and it does not clone or run someone else's hook repository.

Ian floated "agent hook interface" as an expansion. That expansion is not confirmed. The package, the library target, and the binary are all named `ahi`.

The design, the compatibility matrix, and the open decisions are in [docs/SPEC.md](docs/SPEC.md). The release script is described in [docs/release.md](docs/release.md) and is not run by ordinary development.

A proposal to grow ahi from the hook runner into a public agent runtime and coordinator is in [docs/AGENT_RUNTIME.md](docs/AGENT_RUNTIME.md). That document is for review. It is not the design of record, and it does not change the v0.1 through v0.4 contract.

## Installation

### Linux

Prebuilt, statically linked archives are available for **x86_64** and **aarch64** on the [latest release](https://github.com/iancleary/ahi/releases/latest). Cargo is not required.

1. Download the matching `ahi-<version>-linux-<architecture>.tar.gz` and `ahi-<version>-sha256sums.txt`.
2. [Verify the archive's checksum and GitHub build-provenance attestation](#release-assets) before extracting it.
3. Extract the archive and install `ahi` in a directory on your `PATH`, for example:

   ```sh
   tar -xzf ahi-0.3.3-linux-x86_64.tar.gz
   mkdir -p "$HOME/.local/bin"
   install -m 755 ahi "$HOME/.local/bin/ahi"
   ahi --version
   ```

Ensure `$HOME/.local/bin` is on your `PATH`. Substitute `aarch64` for `x86_64` on ARM64 Linux, and the downloaded version for `0.3.3`.

For subsequent releases, run `ahi update`. It requires the [GitHub CLI (`gh`)](https://cli.github.com/) to verify attestations, but does not require Cargo. If upgrading from v0.3.1, install the new archive manually once: that version's updater refuses the newer checksum manifest.

### Windows

A prebuilt **x86_64** archive is available on the [latest release](https://github.com/iancleary/ahi/releases/latest). Cargo is not required.

1. Download `ahi-<version>-windows-x86_64.tar.gz` and `ahi-<version>-sha256sums.txt`.
2. [Verify the archive's checksum and GitHub build-provenance attestation](#release-assets) before extracting it. In PowerShell, `Get-FileHash <archive> -Algorithm SHA256` computes the checksum to compare with the archive's entry in the sums file; use `gh attestation verify` for provenance verification.
3. Extract the archive in PowerShell:

   ```powershell
   tar -xzf ahi-0.3.3-windows-x86_64.tar.gz
   .\ahi.exe --version
   ```

   Substitute the downloaded version for `0.3.3`. Move `ahi.exe` into a directory on your user `PATH` to run `ahi` from any terminal. The binary is unsigned; Windows SmartScreen may prompt on first run.

For subsequent releases, run `ahi update`. It requires the [GitHub CLI (`gh`)](https://cli.github.com/) to verify attestations, but does not require Cargo.

### macOS

Install Rust and Cargo using [rustup](https://rustup.rs/), then install the latest published version:

```sh
cargo install ahi --locked
ahi --version
```

Run `cargo install ahi --locked` again to update. Rust 1.88 or newer is required. Prebuilt macOS archives and `ahi update` support are deferred while GitHub-hosted ARM64 runners lack capacity; `ahi update` currently refuses macOS with a Cargo installation pointer. See the [restoration checklist](docs/macos-update-follow-up.md).

## Build

MSRV is Rust 1.88. A `justfile` wraps the same commands CI runs.

```sh
cargo test --all-features
cargo build --release
./target/release/ahi --version
```

`ahi --version` prints `ahi` followed by the root manifest version and a newline. `scripts/smoke-version.sh` checks that line against the root manifest.

For a real terminal-picker smoke check on POSIX, run `cargo build` then `python3 scripts/smoke-model-picker.py` (Python 3.10+). It uses a disposable state directory and PTY to check catalog browsing, clipboard fallback, checkbox/reorder persistence across restart, stale-save failures, editor suspend/resume, invalid-edit retention, Enter-to-chat, unsupported-provider errors, model switching/cancellation, and transcript retention. It does not use your credentials, editor settings, or the network. Gateway inference has separate loopback HTTP regression tests (`cargo test chat::backend::http`).

`scripts/install-supply-chain-tools.sh` downloads cargo-deny 0.20.2 and cargo-audit 0.22.2 release binaries and checks their sha256. Those versions can read the current RustSec database. The project itself builds with Rust 1.88. `just ci` runs that installer, then `cargo deny check` and `cargo audit`.

### Toolchain versions

Three toolchain numbers are decoupled on purpose.

`rust-version = "1.88"` in the root `Cargo.toml` is the MSRV floor, the oldest Rust ahi promises to build with. ahi's own code needs only 1.83. The floor is set by the dependency tree: `ratatui-core` 0.1.2 and every security-patched `time` release (0.3.47 and later, the RUSTSEC-2026-0009 fix) require 1.88, and `cargo deny check` and `cargo audit` refuse the vulnerable `time` releases, so no allowed dependency set has a lower floor.

CI and the release asset workflows build with a pinned recent stable. Each workflow installs it with the runners' preinstalled rustup and no third-party action, and selects it with a top-level `RUSTUP_TOOLCHAIN`. Local development floats to the latest stable through `rust-toolchain.toml` (`channel = "stable"`). `RUSTUP_TOOLCHAIN` outranks `rust-toolchain.toml` in rustup's precedence, which keeps CI pinned while local floats. Bump the pin and the install steps together when adopting a new stable.

Because ordinary CI builds a recent stable, the MSRV is exercised in exactly one place: the publish workflow checks the declared floor with `cargo +1.88.0 check --all-features --locked` immediately before `cargo publish`. Bump the MSRV only when a security-forced dependency floor or ahi's own code requires it, never because a new stable shipped. A bump is one commit: change `rust-version` in `Cargo.toml`, verify the new floor builds, extend the chain in `docs/SPEC.md`, and record the raise in `CHANGELOG.md` with the next version.

The cache directory is `--cache-dir`, else `AHI_HOME`, else `$XDG_CACHE_HOME/ahi` when that variable is set, else the temporary directory joined with `ahi`. The hook cache does not fall back to a home directory. A store whose `objects/sha256` directory already exists can be opened read-only.

The harness state directory is `--state-dir`, else `AHI_STATE_DIR`, else `$XDG_STATE_HOME/ahi`, else `%LOCALAPPDATA%\ahi` on Windows, else `$HOME/.local/share/ahi`. `models.json` and the OpenAI credential live there. ahi does not read `~/.pi`.

## Commands in this version

Every subcommand also accepts global flags: `-c/--config <path>` selects the config file, `--cache-dir <dir>` overrides `AHI_HOME`, `--state-dir <dir>` overrides `AHI_STATE_DIR`, and `--color auto|always|never` is validated but this version prints plain text either way.

| Command | Behavior |
| --- | --- |
| `ahi run [HOOK]` | Staged files. `[HOOK]` runs only that hook id or alias. `--all-files`, `--files`, and `--from-ref`/`--to-ref` select other sets, `--hook-stage` selects a stage other than pre-commit, and `--commit-msg-filename` feeds the commit-message path to commit-msg and prepare-commit-msg stages. `--format` is `text`, `json`, or `sarif`. `--fail-fast` stops after the first failing hook, and `--verbose`/`-v` shows passing local-hook output. `--fix` rewrites end-of-file, trailing whitespace, and mixed line endings. |
| `ahi list-checks` | Builtin check ids with aliases. `--format` is `text` (default) or `json`. |
| `ahi explain <id>` | What one check reports. |
| `ahi doctor` | Version, git, config path, cache path and whether it is writable, scratch directory, state directory, policy summary. Sandbox detection is not in v0.1. |
| `ahi migrate` | Best-effort suggestion from `.pre-commit-config.yaml`. Prints a report and a suggested config. Does not guarantee feature parity or the same file scope. Does not write unless `--write` is set, and does not replace an existing file unless `--force` is also set. `ahi run` does not use the mapping. |
| `ahi hooks install`, `ahi hooks uninstall` | Exit 1. They are the hook shim commands and are not implemented. Top-level `install` and `uninstall` are not commands. |
| `ahi update` | Replaces the running binary with the latest release from the attested archives. Verifies the sha256sums file and the target archive against their GitHub build-provenance attestations with the `gh` CLI, checks the archive sha256, extracts the single `ahi` member, and atomically renames it over the running binary (moving the running executable aside first on Windows). Refuses any target it cannot verify. Covers linux-x86_64, linux-aarch64, and windows-x86_64; macOS and other targets refuse with a `cargo install ahi` pointer. Does not accept a package name and is not named `self-update`. |
| `ahi tui [--refresh]` | Pick curated models from `models.json`. Load is local-only; `--refresh` explicitly refreshes reference catalogs. Enter saves a selection and opens chat; configured OpenAI-compatible gateways stream real model responses. A pipe prints the load and catalog membership. |
| `ahi login` | Sign in with ChatGPT using the current direct-token browser flow. Prints the URL and accepts a pasted full callback URL when the browser cannot reach ahi. |

In 0.3.0, model catalogs and curated selections are separate. The main picker offers **only** `providers.<id>.models` from `models.json`, in saved order, for OpenAI and every gateway. An omitted list stays empty even after refresh. Reference catalogs never supply picker defaults; their curation view lets you explicitly choose the saved list. See [CHANGELOG.md](CHANGELOG.md) for the breaking gateway-loading change.

The curated picker includes OpenAI and every registered gateway. Provider groups follow the key order of the `providers` object in `models.json`. An implicit, unconfigured OpenAI group starts first; an explicit gateway reorder saves its position too. Highlight a **provider/gateway header** and use **Shift+Up/Down** or **Shift+J/K** to move the whole group and save its order, without changing its models or credentials.

In the picker, `r` refreshes catalogs and `c` opens a provider's curation view. Checked models appear first in saved order, followed by unchecked catalog IDs. **Up/Down or j/k** moves the highlight. **Shift+Up/Down or Shift+j/k** reorders the highlighted checked model and saves. **Space** checks/unchecks and immediately saves. Configured IDs missing from the catalog remain checked and removable. `y` copies an ID (or prints it on exit if no clipboard tool succeeds), and Esc returns to the main picker. Each curated ID shows `listed`, `absent from catalog`, or `catalog unavailable`.

**Enter** on a curated model saves the selection and opens chat. Configured gateways call `<baseUrl>/chat/completions` with streaming enabled and the conversation history. This sends your messages to the selected gateway and may incur charges. `apiKey` and custom header values resolve the marker `gateway` or `$ENV_NAME` at submission time; secrets are not stored in `models.json`. OpenAI browser-login inference (Responses API) is **not implemented yet** and reports an explicit error, never an echo reply. Tools are not executed. The gateway backend can assemble streamed `tool_calls` (including fragmented arguments and parallel calls), validate them, and show a `not executed` notice. It does not advertise tools or send tool results yet: model output is not execution authority. An authorization policy and sandbox must gate a future tool runner. Invalid/incomplete calls fail closed; tool arguments are not printed in notices or parser errors.

Token usage is shown only when returned by the gateway; cost and context size remain unknown. The thinking preference is not sent yet because gateway reasoning capabilities are not modeled. Only one turn runs at a time; Enter preserves the next draft while a reply is active.

Requests refuse redirects and automatic retries, have a 120-second timeout, and cap events at 1 MiB and the total stream at 16 MiB. Exiting chat drops the response receiver but does not guarantee upstream cancellation.

Typing **`/`** at the start of the chat input immediately opens an experimental **Settings** menu above the input. Keep typing to filter, use **Up/Down** to select, **Enter** to apply, and **Esc** to dismiss without clearing your draft. The menu offers `/models`, `/catalog`, `/quit`, thinking levels, and mouse mode, with the current values shown along its bottom edge; settings are local actions, never model prompts. Exact typed commands continue to work. During an active response, Enter preserves the settings draft; press Enter again after the response finishes to apply it. **`$`** is reserved as the future skill-invocation prefix; no skill picker or execution is implemented yet, and it does not open Settings.

**`/models`** (alias **`/model`**) reopens the curated picker across all providers with locally reloaded settings; selecting a model keeps the transcript, and cancelling keeps the active model. **`/catalog`** opens the active provider's full cached catalog for curation, including uncurated models; `r` explicitly refreshes it, Esc returns to the all-provider picker, and `q` returns to chat. No catalog is fetched just by opening it. **`/quit`** (alias **`/exit`**) closes ahi, even during an active response; upstream cancellation is not guaranteed. At the initial picker, `q` exits even if an old selection exists. **Ctrl+D** on empty chat input exits.

Chat has mouse capture **on by default**, with wheel scrolling, clickable jump controls, and app-managed text selection. **Left-drag transcript text** to select it with a background highlight; releasing the drag copies it to the clipboard. **Ctrl+Y** copies the retained selection again; **Esc** clears the highlight without clearing your draft. Copy feedback appears in the input rule, never in the model transcript. If no clipboard adapter succeeds, the selection stays highlighted for retry. Native terminal **Cmd+C** is not the app's copy shortcut; use **`/mouse off`** if you prefer terminal-native drag selection and copying. You can also scroll the chat transcript with **Page Up / Page Down**. **Ctrl+K** (or **Ctrl+Home**) jumps to the top; **Ctrl+L** (or **Ctrl+End**) jumps to the bottom and resumes following streamed output.

Use **`/mouse on`** to re-enable application mouse handling: the wheel scrolls the transcript, and floating **Top**/**Bottom** controls support hover and click. Wheel scrolling preserves selected transcript rows, and click-without-drag still activates the jump controls. Copying rejoins soft-wrapped rows while preserving original newlines and spaces, and handles wide Unicode characters and combining marks. Resizing the transcript width, clearing chat, or changing mouse mode clears selection. Dragging beyond the viewport clamps to its edge; use the wheel to extend a selection across more rows. **`/mouse off`** restores native selection and hides those controls; **`/mouse`** reports the current mode. The mode is session-local and starts on each time you launch chat. Scrolling up keeps your reading position fixed as new output arrives. Plain Home/End still move within the input, and Ctrl+J still inserts a newline.

Explicit checkbox/reorder saves atomically replace `<state>/models.json`, pretty-printing JSON while preserving other providers and settings. Gateway reordering intentionally changes provider key order; model curation preserves it. Stale external edits or save failures leave the file and loaded list unchanged; press `e` to review/reload before retrying. Merely browsing does not create a file; an explicit checkbox action can.

`e` opens `models.json` in `$VISUAL` or `$EDITOR` (notepad fallback on Windows). Other systems ask for an editor when neither is set. Editor-only reload never rewrites the editor's bytes and never fetches. Invalid edits retain the previous registry and display the file path and line.

For example, save this in the state directory's `models.json` (replace the example id with the model you want):

```json
{
  "providers": {
    "openai": {
      "baseUrl": "https://api.openai.com/v1",
      "api": "openai-completions",
      "models": [{"id": "gpt-4.1"}]
    }
  }
}
```

**OpenAI login now uses the current Sign in with ChatGPT direct-token flow**, not the older Codex fixed-client flow. `ahi login` binds `http://127.0.0.1:1455/auth/callback` before opening the browser, requests an issued client for this installation and the `https://api.openai.com/v1` resource with `chatgpt.tokens.use.direct`. When automatic callback is unavailable, paste the full final callback URL, including `state` and `client_id`; a bare code is not sufficient. Old credentials remain untouched until a new login succeeds; explicit refresh asks you to run `ahi login` to migrate. Browser tokens are never forwarded to proxies: a conflicting OpenAI `baseUrl` causes an error. Register proxies as separate gateways.

Explicit OpenAI refresh attempts `GET https://api.openai.com/v1/models` only with new direct-token credentials. **A live request with a new direct-token credential returned HTTP 200 and model metadata.** That response used `models[].slug`; ahi accepts this shape as well as the standard `data[].id` format, preserving every reported ID in order. The earlier HTTP 403 was from an old Codex-flow credential, not the new grant. A 403 preserves the catalog and curated IDs and reports unavailable discovery. Expiry is checked before requests; authentication failures permit at most one renewal and retry. Requests refuse redirects, have a 15-second timeout and a 1 MiB response cap. Pi uses this grant for Responses, not Decisions; ahi's OpenAI browser-login inference is still unimplemented. Configured gateway inference uses a separate Chat Completions backend. `auth signed-in` means only a credential file exists.

Gateway catalog refresh requests `GET <origin>/agent/models.json`, stripping the path from the configured `baseUrl`. This is a gateway-specific discovery convention, not an OpenAI or industry-standard catalog endpoint. The gateway retains `/pi/agent/model.json` as a legacy alias; ahi uses only the canonical route. Gateways without this catalog route can still serve inference for explicitly configured model IDs.

`ahi tui` never fetches on startup. `ahi tui --refresh` explicitly requests refreshes, preserves old catalogs on failure, and in a pipe exits 1 if any refresh failed while still printing the local load. Catalogs are atomically replaced objects containing provider, source URL, retrieval timestamp, and reported ids; old array-format catalogs remain readable. `retrieval time unknown (old catalog format)` means that the cached array lacks a timestamp, not that authentication is old. Refresh never edits the curated file.

Checkers are `repo: builtin`. `ahi lock`, `ahi verify`, `ahi fetch`, `update`, `self-update`, `install`, and `uninstall` are not commands. The hook shim commands are `ahi hooks install` and `ahi hooks uninstall`, and they exit 1. `repo: local` with `language: system` or `language: script` runs when `ahi.policy.yaml` lists that hook id and the exact `entry` string. The entry is split without a shell. A missing policy prints the snippet to add and does not run the command. Other local languages fail the hook and say `not supported by ahi`. `repo: meta` fails with an explicit error. A URL repo fails the hook and is not fetched. Nothing is skipped silently.

ahi runs tools that are already installed. It can pass a toolchain manager the variables it needs to find itself. It does not install, update, resolve, or cache a toolchain or dependencies for a local hook.

| | Names |
| --- | --- |
| Passed by default, if set | `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, plus `PATH`, `HOME`, and the other base names in the spec. `PRE_COMMIT=1` is set. |
| Policy only (`toolchain_env`) | `GOROOT`, `GOPATH`, `VIRTUAL_ENV`, `NVM_DIR`. Not enabled by default. `NVM_DIR` does not enable Node and does not run `npm install`. |
| Never passed | Names containing `TOKEN`, `SECRET`, `PASSWORD`, or `KEY`, and names starting with `AWS_`, `AZURE_`, or `GITHUB_`. Listing one in the policy is an error. |

`ahi explain` prints those names. The audit line records the names passed to each local hook and does not record values. A global `toolchain_env` list applies to every local hook. A `toolchain_env` list on one `trusted_system_hooks` entry applies only to that id and entry.

Builtin checks are report-only unless `--fix` is passed. `--fix` rewrites only `ahi/end-of-file`, `ahi/trailing-whitespace`, and `ahi/line-endings`, then the hook fails because the file changed. A hook argument `--fix` does the same for those three. `--fix=no` on mixed line endings stays report-only. The write is a temp file in the same directory, fsynced, then renamed. Symlinks are refused. Paths outside the selected set are refused. A file with unstaged changes is left unchanged. ahi does not stash.

`ahi migrate` is a reading aid, not upstream `migrate-config`. It does not rewrite `sha`, `python_venv`, or legacy stage names, and it does not fetch a `rev`. Hooks with an obvious builtin id are suggested as `repo: builtin`. `repo: local` system and script hooks are copied with the policy snippet they need. Everything else is listed under "Not ported" with a reason. A run-time substitution of remote hook ids was removed; native checks run only from `repo: builtin`.

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

## Examples

`examples/starter` is builtins plus the two local Rust hooks, with a matching policy. `examples/ian-repos` is the same Rust hooks and the three pre-commit-hooks checks expressed as `repo: builtin`. Both expect:

```sh
ahi run --fix
```

`examples/starter/pre-commit` is a sample hook:

```sh
#!/bin/sh
exec ahi run --fix
```

## Release assets

A published GitHub release for tag `v<version>` carries four assets. `<version>` is the SemVer string without the leading `v` (tag `v0.1.1` uses `0.1.1`).

- `ahi-<version>-linux-x86_64.tar.gz`
- `ahi-<version>-linux-aarch64.tar.gz`
- `ahi-<version>-windows-x86_64.tar.gz`
- `ahi-<version>-sha256sums.txt`

Each archive contains one `ahi` executable at the archive root (`ahi.exe` on Windows) and no other members. The Linux archives are statically linked and bit-reproducible on their runner; the Windows archive is built natively and is not promised to be bit-reproducible. On macOS use `cargo install ahi` until [native updates are restored](docs/macos-update-follow-up.md). The sums file is `sha256sum` output for those three archives in the order listed above.

From the directory that contains the downloaded files, for release `0.1.1`:

```sh
sha256sum --check ahi-0.1.1-sha256sums.txt
gh attestation verify ahi-0.1.1-linux-x86_64.tar.gz --repo iancleary/ahi
gh attestation verify ahi-0.1.1-linux-aarch64.tar.gz --repo iancleary/ahi
gh attestation verify ahi-0.1.1-windows-x86_64.tar.gz --repo iancleary/ahi
gh attestation verify ahi-0.1.1-sha256sums.txt --repo iancleary/ahi
```

`sha256sum --check` uses the filenames recorded in the sums file, so the archives have to sit beside that file. Replace `0.1.1` with the release that was downloaded.

v0.1.0 and v0.1.1 were published before this workflow existed, so they have no assets until the workflow is dispatched. After `.github/workflows/release_binary_assets.yml` is on `main`:

```sh
gh workflow run release_binary_assets.yml --repo iancleary/ahi --ref main -f release_tag=v0.1.1
```

The same command with `release_tag=v0.1.0` backfills that release. Dispatch from `main`. The tag that is built does not need to contain the workflow file. The workflow refuses a ref that is not a tag, and it refuses to replace an asset whose bytes differ. The next release published after the workflow is on `main` receives the three assets from the `release` trigger, without a manual run.

Archive layout, runners, and the reproducibility flags are in [docs/release.md](docs/release.md).

## License

MIT, matching the `LICENSE` file already in this repository. Dual-licensing is an open decision and is not applied here.
