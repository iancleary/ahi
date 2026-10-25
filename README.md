# ahi

`ahi` is a Rust library and command-line tool that reads the [pre-commit](https://pre-commit.com/) configuration files (`.pre-commit-config.yaml` and `.pre-commit-hooks.yaml`) and runs fast, offline checks. v0.1 parses that config, filters files the way pre-commit does, and executes built-in checks. It does not download runtimes and it does not run hooks written for other languages yet.

Ian floated "agent hook interface" as an expansion. That expansion is not confirmed. The package, the library target, and the binary are all named `ahi`.

The design, the compatibility matrix, and the open decisions are in [docs/SPEC.md](docs/SPEC.md). The release script is described in [docs/release.md](docs/release.md) and is not run by ordinary development.

## Build

MSRV is Rust 1.83. A `justfile` wraps the same commands CI runs.

```sh
cargo test --all-features
cargo build --release
./target/release/ahi --version
```

`ahi --version` prints `ahi 0.1.0` and a newline. `scripts/smoke-version.sh` checks that line against the root manifest.

`scripts/install-supply-chain-tools.sh` downloads cargo-deny 0.20.2 and cargo-audit 0.22.2 release binaries and checks their sha256. Those versions can read the current RustSec database. The project itself still builds with Rust 1.83. `just ci` runs that installer, then `cargo deny check` and `cargo audit`.

The cache directory is `--cache-dir`, else `AHI_HOME`, else `$XDG_CACHE_HOME/ahi` when that variable is set, else the temporary directory joined with `ahi`. ahi does not write under a home directory. A store whose `objects/sha256` directory already exists can be opened read-only.

## Commands in this version

| Command | Behavior |
| --- | --- |
| `ahi run` | Staged files. `--all-files`, `--files`, and `--from-ref`/`--to-ref` select other sets. `--format` is `text`, `json`, or `sarif`. `--fix` rewrites end-of-file, trailing whitespace, and mixed line endings. |
| `ahi list-checks` | Builtin check ids. |
| `ahi explain <id>` | What one check reports. |
| `ahi doctor` | Version, git, config path, cache path and whether it is writable, scratch directory, policy summary. Sandbox detection is not in v0.1. |
| `ahi migrate` | Best-effort suggestion from `.pre-commit-config.yaml`. Prints a report and a suggested config. Does not guarantee feature parity or the same file scope. Does not write unless `--write` is set, and does not replace an existing file unless `--force` is also set. `ahi run` does not use the mapping. |
| `install`, `uninstall`, `lock`, `verify`, `fetch`, `autoupdate` | Exit 1. They are specified and not implemented. |

`repo: local` with `language: system` or `language: script` runs when `ahi.policy.yaml` lists that hook id and the exact `entry` string. The entry is split without a shell. A missing policy prints the snippet to add and does not run the command. Other local languages fail the hook. `repo: meta` and remote URLs fail with an explicit error and are not fetched. Languages planned for v0.3 say `not supported until v0.3`. Other languages, including Node, say `not supported by ahi`. Nothing is skipped silently.

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

## License

MIT, matching the `LICENSE` file already in this repository. Dual-licensing is an open decision and is not applied here.
