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
| `ahi run` | Staged files. `--all-files`, `--files`, and `--from-ref`/`--to-ref` select other sets. `--format` is `text`, `json`, or `sarif`. |
| `ahi list-checks` | Builtin check ids. |
| `ahi explain <id>` | What one check reports. |
| `ahi doctor` | Version, git, config path, cache path and whether it is writable, scratch directory. Sandbox detection is not in v0.1. |
| `install`, `uninstall`, `lock`, `verify`, `fetch`, `autoupdate`, `migrate` | Exit 1. They are specified and not implemented. |

A hook with `repo: local`, `repo: meta`, or a remote URL fails the run with an explicit error. Languages planned for v0.3 say `not supported until v0.3`. Other languages, including Node, say `not supported by ahi`. Nothing is skipped silently.

Builtin checks are report-only. They do not rewrite the worktree.

```yaml
repos:
  - repo: builtin
    hooks:
      - id: ahi/trailing-whitespace
      - id: ahi/end-of-file
      - id: ahi/dockerfile
      - id: ahi/github-actions
      - id: ahi/shell-strict
```

## License

MIT, matching the `LICENSE` file already in this repository. Dual-licensing is an open decision and is not applied here.
