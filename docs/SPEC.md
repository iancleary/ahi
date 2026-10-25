# ahi specification

Status: design of record for v0.1 through v0.4. This document is the compatibility, security, and architecture contract. It is not a release announcement. Nothing in this repository is published to crates.io, tagged, or cut as a GitHub Release by this work.

The binary, the Rust library target, and the crates.io package are all named `ahi`. Callers write `use ahi::...`. Ian floated "agent hook interface" as an expansion and also noted the fish-hook pun. That expansion is not confirmed, so this spec does not treat it as the official name. The registry check is in [Open decisions](#open-decisions).

Research date for the citations below: 2026-10-25. Upstream facts are taken from the fetched pages and source files listed in [Sources](#sources). Anything this author did not re-check is marked **Unverified**.

## Inferred context

The following is **inferred** from the task brief, not from a statement Ian wrote in this repository. Treat it as design input, not as a confirmed preference.

- Development happens on Windows, WSL (Arch), macOS, and native Windows, and those environments must behave the same way.
- Machines and repos are under Ian's own administration. There is no third-party hook-runner service in the design (in particular, ahi does not integrate with pre-commit.ci).
- Container images stay minimal. Node.js is deliberately absent from those images, so a hook runner that downloads Node is not acceptable.
- Supply-chain rules already used elsewhere: every downloaded tool is pinned to an exact version with a sha256 per architecture; upstream signatures or attestations are verified where they exist; installs are not `curl | sh`; failure is closed; checksums live in version files.
- An earlier note suggested calendar versioning. Ian's `rfconversions` repository cuts explicit SemVer releases. Which scheme ahi uses is an open decision. The repo contains a `justfile`. Implementation language is Rust.
- Repositories that will use ahi contain shell, Dockerfiles, GitHub Actions workflows, TypeScript, Rust, Go, and Python.

The deployment Ian described (OpenShell sandboxes, a read-only baked cache, git inside the sandbox) is stated in [Running inside OpenShell](#running-inside-openshell). It is his brief plus the NVIDIA docs fetched for this spec, not an inference from the earlier host-sandbox note.

Observed, not inferred: the repository root already contains an MIT `LICENSE` with `Copyright (c) 2026 Ian Cleary`. That file is the license for this build. Changing it is an open decision.

## Running inside OpenShell

Ian runs agents inside NVIDIA OpenShell sandboxes. This section is the deployment model for ahi. It replaces the earlier assumption that ahi's own host-side sandbox, and a host-side import of commits, is the primary way the tool is used.

What Ian stated:

- The filesystem policy is fixed when the sandbox is created. In his policy, system paths are read-only, and the only read-write paths are the workspace `/sandbox` and `/tmp`.
- The process is non-root. Landlock and seccomp are in effect. Outbound network goes through a policy proxy and is deny-by-default.
- Credentials are injected as placeholders by providers. He uses that so `git clone`, `commit`, `pull`, and `push` can run inside the sandbox with the provider's placeholder GitHub token. He does not need ahi to import a commit from the host or to sign it on the host.

What the OpenShell docs fetched on 2026-10-25 say. These sentences are the product's, and they are not identical to Ian's policy:

- A filesystem policy is static for the life of the sandbox. Paths that are not listed are inaccessible. `include_workdir` defaults to true when `filesystem_policy` is omitted and to false when that section is present. `read_write` cannot be `/`. The schema caps the list at 256 paths. Paths are absolute, must not contain `..`, and are limited to 4096 bytes. Landlock `compatibility` is `best_effort` (the default) or `hard_requirement`. The mandatory baseline expects Landlock ABI 3 (Linux 6.2+). Sources: the [policy schema](https://docs.nvidia.com/openshell/how-it-works/policies/schema) and the [reference schema](https://docs.nvidia.com/openshell/reference/policy-schema).
- The product default is not "read-write only `/sandbox` and `/tmp`". When `filesystem_policy` is omitted, the working directory is writable along with `/tmp`, plus the documented read-only baseline. The sandboxes policy page also says that proxy mode adds a baseline which includes `/sandbox` and `/tmp` as read-write and several system paths as read-only, and that `/app` is added only when it exists in the image. `/sandbox` as the workspace path in Ian's policy is his choice. This spec does not call that the OpenShell default. Sources: [default policy](https://docs.nvidia.com/openshell/how-it-works/policies/default-policy) and [sandboxes policies](https://docs.nvidia.com/openshell/latest/sandboxes/policies). An earlier fetch of the sandboxes policies URL returned 404; a later fetch of the same URL succeeded and is the copy cited here.
- Seccomp is automatic, not a user knob, and has two phases (a startup prelude, then the runtime profile). The runtime profile denies `seccomp(SECCOMP_SET_MODE_FILTER)` with `EPERM`. It also denies mount, `pivot_root`, bpf, ptrace, and user-namespace creation. Socket families are limited to `AF_UNIX`, `AF_NETLINK` (`NETLINK_ROUTE` only), and brokered `AF_INET` / `AF_INET6`. Source: [security best practices](https://docs.nvidia.com/openshell/security/best-practices).
- The network path is a CONNECT proxy with OPA, default deny, and it can change at runtime. Providers put opaque placeholders in the environment and substitute the real credential only when both the network policy and the credential binding (host, port, and path) allow the request. Raw TLS skip and non-HTTP tunnels do not rewrite credentials. The GitHub provider can describe `GITHUB_TOKEN` and the API endpoints. Sources: [providers overview](https://docs.nvidia.com/openshell/how-it-works/providers/overview) and the providers v2 notes in the OpenShell repository (`docs/sandboxes/providers-v2.mdx`).

What that means for ahi:

1. **The image build is the online step.** `ahi` and the tool cache are baked into the image while the build still has network. Every download is pinned to an exact version and a per-architecture sha256. That bake is the same contract as `ahi fetch`. At runtime the store is read-only. `ahi run` verifies cached objects against `ahi.lock` and does not open a network connection.
2. **No writable home.** Cache and scratch locations are `--cache-dir`, else `AHI_HOME`, else `$XDG_CACHE_HOME/ahi` when that variable is set, else `std::env::temp_dir()/ahi`. Hook scratch uses `std::env::temp_dir()` too. A store whose object directory already exists opens read-only when it cannot be written. `put` fails closed in that case. `get` re-hashes.
3. **In-sandbox hooks are advisory.** The sandbox user can edit `.pre-commit-config.yaml`, `ahi.policy.yaml`, and the arguments passed to `ahi`. A hook run inside that sandbox is fast feedback. CI running `ahi` on the commit under review is the backstop. The threat model states this in one place so it is not implied by the sandbox marketing.
4. **Do not nest by default.** v0.4 detects that the process is already inside an OpenShell sandbox (or another sandbox with the same constraints) and does not apply a second Landlock or seccomp filter. Host-side backends remain an option for a machine that is not already sandboxed. The documented `EPERM` on `SECCOMP_SET_MODE_FILTER` means a nested seccomp filter is expected to fail. **Unverified:** this project did not run ahi inside OpenShell, did not try to nest Landlock or seccomp, and did not find OpenShell docs for macOS or native Windows.
5. **Git stays inside the sandbox.** `--from-ref` and `--to-ref` stay. ahi does not import a host commit and does not require host-side signing. Two lower-priority ideas remain, and they are not v0.1: load the config and the lockfile from a git ref, and refuse a config that the checked range itself modified.
6. **Placeholder credentials for git.** Ian's workflow is git over HTTPS inside the sandbox, using the provider placeholder. The provider docs describe HTTP placeholder substitution. An end-to-end `git push` with a placeholder token was not executed for this spec. **Unverified.**

## Goals

1. Read the pre-commit framework's configuration files (`.pre-commit-config.yaml` and `.pre-commit-hooks.yaml`) and run a useful subset of what they describe, including the same file-selection and file-filtering model.
2. Fail closed on supply-chain and sandbox boundaries that upstream pre-commit leaves open: floating refs, runtime downloads, unhashed extra dependencies, `language: system` / `script`, and network during `run`.
3. Ship fast offline checks for the repositories described above, addressable as `repo: builtin` with ids such as `ahi/trailing-whitespace`.
4. Expose a small Rust library (check trait, runner, content-addressed store, text/JSON/SARIF formatters) and an `ahi` binary.
5. Keep the dependency tree small, the MSRV declared, and CI pinned and least-privilege.

## Non-goals

- A drop-in bug-for-bug reimplementation of every pre-commit language, hook, and command. [prek](https://prek.j178.dev/dev/) already pursues that, and it auto-installs toolchains. ahi will not.
- Publishing to crates.io, cutting a release, pushing a git tag, or running `scripts/cut-release.sh`. The release script and the tag-gated publish workflow are in the tree so a later release has a path. This work does not invoke them. SemVer versus calendar versioning is an open decision.
- Hosting or calling pre-commit.ci, or any other third-party CI service. **Inferred.**
- Executing arbitrary hook languages. `language: system` and `language: script` run only when `ahi.policy.yaml` pins the hook id and the exact `entry`. Other languages fail with an explicit error. Remote repos are not fetched and are not substituted at run time.
- Rewriting the worktree except for three explicit fixers. `ahi run --fix` rewrites end-of-file, trailing whitespace, and mixed line endings. It does not stash. A file with unstaged changes is left alone. Other builtins stay report-only. The partial-commit stash protocol is still not implemented.
- A new configuration language. `prek.toml` and `lefthook.yml` / `hk.pkl` are out of scope. ahi reads pre-commit YAML, plus an optional repo-local policy file that upstream pre-commit ignores.

## Prior art

### pre-commit

[pre-commit](https://pre-commit.com/) (Python, current release [v4.7.0](https://github.com/pre-commit/pre-commit/releases/tag/v4.7.0), published 2026-10-13) is a multi-language package manager for Git hooks. It clones hook repositories, builds an isolated environment per language, and runs hooks on staged files. The project intentionally has no "floating latest" rev: `rev` is cached as an immutable ref, and moving branches are unsupported ([docs](https://pre-commit.com/#using-the-latest-version-for-a-repository)). It will, however, download language runtimes when they are missing. Confirmed in source, not only in the intro text:

- `language: node` installs a prebuilt Node through `python -m nodeenv --prebuilt` ([`languages/node.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/languages/node.py)).
- `language: rust` downloads `rustup-init` from `https://sh.rustup.rs` (or `https://win.rustup.rs/x86_64` on Windows) when `rustup` is absent, then `cargo install --bins` without `--locked` ([`languages/rust.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/languages/rust.py)).
- `language: golang` downloads a Go toolchain archive when needed ([`languages/golang.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/languages/golang.py)).

That download-on-demand behavior is the main incompatibility ahi is built to refuse.

### prek

[prek](https://github.com/j178/prek) (Rust, MIT, about 8679 GitHub stars on 2026-10-25) is a single static binary positioned as a faster drop-in for pre-commit. From its [compatibility](https://prek.j178.dev/dev/compatibility/) and [differences](https://prek.j178.dev/diff/) docs:

- Existing `.pre-commit-config.yaml` / `.yml` files and `.pre-commit-hooks.yaml` manifests work.
- It adds `repo: builtin`, native Rust reimplementations of common hooks, `prek.toml`, workspace mode, hook priority, and glob forms of `files` / `exclude`.
- It installs managed toolchains (Python via uv, Node, Bun, Deno, Go, Rust, Ruby, and others) and shares environments across hooks.
- `pre-commit hazmat` is not implemented. `migrate-config` is not the same command: prek's migration path targets `prek.toml`.

ahi is not a fork of prek and does not depend on it. The `repo: builtin` idea is the same shape, and ahi should stay readable by people who have used prek, but ahi will not download a toolchain to achieve compatibility.

### lefthook

[lefthook](https://github.com/evilmartians/lefthook) (Go, MIT; docs at [lefthook.dev](https://lefthook.dev/)) is a dependency-free Git hook manager with its own `lefthook.yml`. It installs hook shims, filters with globs and regexes, can run jobs in parallel (`parallel: true`, default false), supports remote shared configs ([remotes](https://lefthook.dev/configuration/remotes/)), and can restage fixer output. It is not a pre-commit config implementation. ahi does not read `lefthook.yml`.

### hk

[hk](https://github.com/jdx/hk) (docs at [hk.jdx.dev](https://hk.jdx.dev/why-hk.html)) runs project checks and fixers concurrently, with per-file read/write locks so two fixers do not clobber the same file. Configuration is Pkl (`hk.pkl`), not pre-commit YAML. It can stash unstaged changes during `pre-commit` (`stash = "git"`) and has a `hk migrate pre-commit` path from the pre-commit framework. Tool installation is delegated to mise or the existing `PATH`. ahi does not read `hk.pkl`. The stash and "check versus fix" split are design references. v0.1 checks by default. `ahi run --fix` rewrites three whitespace checks and refuses a dirty unstaged file instead of stashing.

### Where ahi differs

| | pre-commit | prek | lefthook | hk | ahi |
| --- | --- | --- | --- | --- | --- |
| Config | `.pre-commit-config.yaml` | that, plus `prek.toml` | `lefthook.yml` | `hk.pkl` | pre-commit YAML, plus optional `ahi.policy.yaml` |
| Remote hooks | clone at `rev` | clone at `rev` | remotes | imports | only after `ahi fetch`, locked to a commit and a tree digest |
| Runtimes | downloads Node, Rust, Go, others | downloads several toolchains | uses what you run | mise or `PATH` | allowlisted, already installed, never downloaded |
| Network on `run` | yes, on first use | yes, on first use | when fetching remotes | when mise fetches | no |
| Native checks | no (the separate pre-commit-hooks repo) | `repo: builtin` and a fast path | no | builtins | `repo: builtin` |
| Sandbox | no | not claimed in the docs read for this spec | no | no | v0.4 detects an existing sandbox and does not nest; host backends are optional |
| Parallel hook execution | batches files within one hook; hooks themselves run serially | hooks can run in parallel by priority | optional | default, with file locks | hooks serial in v0.1 (`fail_fast` preserved). File-level locking is only relevant once fixers exist |

## Compatibility matrix

"Schema 4.7.0" means the config and manifest schema implemented by pre-commit as of the `main` sources fetched for this spec and the v4.7.0 release. ahi's own version number is not a pre-commit version. `minimum_pre_commit_version` is compared against the schema level ahi claims to implement (initially `4.7.0`), using the same dotted-integer comparison as [`check_min_version`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py). A requirement newer than that schema level is a hard error.

| Surface | v0.1 | Later | Stricter than upstream, or unsupported |
| --- | --- | --- | --- |
| `.pre-commit-config.yaml` | parsed and enforced |  | Unknown keys are errors. Upstream logs a warning ([`warn_unknown_keys_root`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py)). |
| `.pre-commit-config.yml` | parsed if `.yaml` is absent |  | Extension. Upstream's constant is `.yaml` only ([`constants.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/constants.py)). If both names exist, ahi errors. |
| `.pre-commit-hooks.yaml` | parsed by the library | used when fetching remote repos (v0.2+) | Same strictness: unknown keys error. |
| Top-level `repos`, `files`, `exclude`, `fail_fast`, `default_stages`, `default_install_hook_types`, `default_language_version`, `minimum_pre_commit_version` | parsed; `files` / `exclude` / `fail_fast` / `default_stages` affect `ahi run` | install uses `default_install_hook_types` | `default_stages` default remains "all stages", matching upstream. |
| `ci:` | accepted as a mapping and retained on the config struct | not acted on | ahi does not talk to pre-commit.ci. Unknown nested keys are errors, unlike upstream, which accepts any dict. Schema of the mapping matches [pre-commit.ci](https://pre-commit.ci/) / [`pre_commit_ci_config.py`](https://github.com/pre-commit-ci/pre-commit-ci-config/blob/main/pre_commit_ci_config.py). |
| `repo` URL | parsed, not cloned, not substituted | v0.2 lock + fetch, v0.3 execute | No network on `run`. A remote hook fails with an explicit error. `ahi migrate` may suggest a builtin id; `ahi run` does not apply that table. |
| `repo: local` | `language: system` and `script` run when the policy pins the id and the exact `entry` | other languages in v0.3 | The entry is split without a shell. A bare id does not authorize. `args` are not part of the pin. |
| `repo: meta` | parsed | not scheduled | The three meta hooks (`check-hooks-apply`, `check-useless-excludes`, `identity`) are recognized. v0.1 does not run them. `entry` remains forbidden, matching upstream. |
| `repo: builtin` | runs |  | ahi extension, same sentinel idea as prek. No `rev` key. |
| `rev` | required for URL repos, stored as a string | v0.2: must resolve to a full 40-hex commit recorded in `ahi.lock` | Tags and branches are not accepted at `ahi run` once a lockfile is required (v0.2). v0.1 does not fetch, so it cannot resolve tags. |
| Hook fields `id`, `alias`, `name`, `language`, `language_version`, `entry`, `files`, `exclude`, `types`, `types_or`, `exclude_types`, `args`, `stages`, `always_run`, `fail_fast`, `pass_filenames`, `require_serial`, `verbose`, `description`, `log_file`, `additional_dependencies`, `minimum_pre_commit_version` | parsed and validated | execution uses them when the language is implemented | See filtering below. `additional_dependencies` are not installed in v0.1. |
| Stages and hook types | `--hook-stage` filters hooks. Default stage is `pre-commit` | `ahi install` writes shims | Legacy names `commit`, `push`, `merge-commit` are rewritten to `pre-commit`, `pre-push`, `pre-merge-commit` and then rejected with a migration hint. Upstream rewrites and warns ([`transform_stage`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py)). ahi does not silently accept them. |
| `manual` stage | recognized; runs only with `--hook-stage manual` |  | Matches upstream. |
| File discovery: staged, `--all-files`, `--files`, `--from-ref`/`--to-ref` | implemented |  | Commands match [`git.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/git.py) (see [File discovery](#file-discovery)). |
| File filtering | implemented |  | Regex dialect is stricter (see [Filtering](#filtering)). |
| `language: fail` and `language: pygrep` | recognized, not executed | v0.3, no runtime needed | Good candidates to pull forward. Not in the v0.1 execution set. |
| Languages other than the v0.3 set | parsed if they are real pre-commit language names | unsupported | A hook whose language is outside the implemented set fails that hook with `not supported until v0.3` (or "not planned", for languages listed as unsupported). It is not skipped. |
| `SKIP` | honored (comma-separated hook ids and aliases) |  | Matches [`_get_skips`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/commands/run.py). |
| xargs-style batching | used for `repo: local` system and script hooks. Batches run one after another | same partition for later languages | Builtin checks run in-process on the full filtered list. `SC_ARG_MAX` is not read. |
| Partial-commit stash | not invoked | required before any check may write the worktree | Specified below so the later implementation has a contract. |
| Git hook shim, `install`, `uninstall`, `init-templatedir`, git 2.54 `hook.<name>.event` config | not installed | after the runner is safe to put on `git commit` | v0.1 `install` / `uninstall` exit with a clear "not implemented yet" error. |
| `autoupdate`, `try-repo`, `migrate-config`, `clean`, `gc`, `sample-config`, `validate-config`, `validate-manifest`, `hazmat` | corresponding ahi commands that exist early exit with the phase that will implement them. `ahi migrate` is a different command (best-effort suggestion, not upstream `migrate-config`) | see roadmap | ahi does not implement `hazmat` (`cd`, `ignore-exit-code`, `n1`). Those entry prefixes are a hard error if encountered, not a silent pass. Upstream documents them as usually a bad idea ([docs](https://pre-commit.com/#pre-commit-hazmat)). |
| Worktree rewriting / formatters | `ahi run --fix` for end-of-file, trailing whitespace, and mixed line endings. No stash. Unstaged files are refused. Writes are atomic and do not follow symlinks | stash protocol still later | Other builtins, including the UTF-8 BOM check, stay report-only. |

Exit codes for `ahi run`:

| Code | Meaning |
| --- | --- |
| 0 | Every selected hook passed or was skipped with no findings. |
| 1 | A hook failed, the config is invalid, or a selected hook is unsupported. This is the "expected failure" class, matching pre-commit's `1`. |
| 2 | Usage error (clap). |
| 3 | Internal error (a panic is still a panic; a caught bug returns 3). Matches pre-commit's unexpected-error code. |
| 130 | Interrupted. |

## Config schema

YAML 1.1 as commonly written by pre-commit users (block mappings, folded strings). Duplicate keys are errors. Tabs in indentation are errors. `files` and `exclude` are strings, not globs. ahi does not accept prek's glob-mapping extension, so a file shared with upstream pre-commit stays valid upstream.

### Top level

| Key | Required | Default | Notes |
| --- | --- | --- | --- |
| `repos` | yes |  | Array. May be empty, which runs nothing and exits 0. |
| `default_install_hook_types` | no | `[pre-commit]` | Each value is a hook type, not `manual`. |
| `default_language_version` | no | `{}` | Map of language name to string. Unknown language names are errors. |
| `default_stages` | no | all stages, including `manual` | Applied only to hooks that set neither manifest nor config `stages`. Upstream's manifest default for `stages` is `[]` and the config layer replaces an empty list with all stages. ahi does the same when materializing a hook. |
| `files` | no | `""` | Python `re.search` of `""` matches every filename. ahi matches every filename. |
| `exclude` | no | `"^$"` | Matches only the empty string, so ordinary paths are kept. |
| `fail_fast` | no | `false` | Stop after the first hook failure. A hook-level `fail_fast: true` does the same for that hook. |
| `minimum_pre_commit_version` | no | `"0"` | Compared to schema level `4.7.0`, not to the ahi package version. |
| `ci` | no | absent | See below. |

### `ci` (parsed, not executed)

Defaults below match [pre-commit.ci](https://pre-commit.ci/) when the key is present. When the key is absent, ahi stores `None` and does not invent a schedule.

| Key | Default when `ci:` is present |
| --- | --- |
| `autofix_commit_msg` | the upstream multi-line default |
| `autofix_prs` | `true` |
| `autoupdate_branch` | `""` (rejected if equal to `pre-commit-ci-update-config`, matching upstream) |
| `autoupdate_commit_msg` | `[pre-commit.ci] pre-commit autoupdate` |
| `autoupdate_schedule` | `weekly` (`weekly`, `monthly`, `quarterly`) |
| `skip` | `[]` of hook ids |
| `submodules` | `false` |

ahi does not autofix pull requests and does not run on a weekly schedule. The block is parsed so that sharing a file with pre-commit.ci users is not a syntax error.

### Repos

| `repo` value | `rev` | Hook object |
| --- | --- | --- |
| URL string (`git clone` input) | required string | remote hook: `id` required; other manifest fields optional (they override the manifest once one is fetched). v0.1 has no manifest, so only `id` and the override fields are known. |
| `local` | forbidden | full manifest hook, all required manifest fields required |
| `meta` | forbidden | `id` is one of the three meta ids; `entry` forbidden; `language` if set must be `unsupported` |
| `builtin` | forbidden | `id` is a builtin id or a documented alias. `entry` and `language` forbidden. Filter fields and `args` allowed. |

`repo: builtin` is the v0.1 execution path:

```yaml
repos:
  - repo: builtin
    hooks:
      - id: ahi/trailing-whitespace
      - id: ahi/end-of-file
        exclude: ^vendor/
```

### Manifest hook (`.pre-commit-hooks.yaml` and `repo: local`)

Required: `id`, `name`, `entry`, `language`.

Optional, with upstream defaults from [`MANIFEST_HOOK_DICT`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py):

| Key | Default |
| --- | --- |
| `alias` | `""` |
| `files` | `""` |
| `exclude` | `"^$"` |
| `types` | `[file]` |
| `types_or` | `[]` |
| `exclude_types` | `[]` |
| `additional_dependencies` | `[]` |
| `args` | `[]` |
| `always_run` | `false` |
| `fail_fast` | `false` |
| `pass_filenames` | `true` |
| `description` | `""` |
| `language_version` | `default` |
| `log_file` | `""` |
| `require_serial` | `false` |
| `stages` | `[]`, later replaced by `default_stages` when empty |
| `verbose` | `false` |
| `minimum_pre_commit_version` | `"0"` |

Language names, from [`all_languages.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/all_languages.py): `conda`, `coursier`, `dart`, `docker`, `docker_image`, `dotnet`, `fail`, `golang`, `haskell`, `julia`, `lua`, `node`, `perl`, `pygrep`, `python`, `r`, `ruby`, `rust`, `swift`, `unsupported`, `unsupported_script`.

Aliases accepted on input and rewritten, matching [`_translate_language`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py): `system` becomes `unsupported`, `script` becomes `unsupported_script`. After rewriting, both are still denied unless the policy trusts that hook id.

### Stages

Hook types ([`HOOK_TYPES`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py)): `commit-msg`, `post-checkout`, `post-commit`, `post-merge`, `post-rewrite`, `pre-commit`, `pre-merge-commit`, `pre-push`, `pre-rebase`, `prepare-commit-msg`.

Stages are those types plus `manual`.

Hooks that do not operate on files, per the upstream docs: `post-checkout`, `post-commit`, `post-merge`, `post-rewrite`, `pre-rebase`. They need `always_run: true` or they are skipped. `commit-msg` and `prepare-commit-msg` receive the commit-message file. v0.1 `ahi run` implements file stages (`pre-commit`, `pre-merge-commit`, `pre-push`, `manual`). The other stages are parsed and selectable, but a builtin file check selected in a non-file stage reports "no files" unless `always_run` is set, matching upstream's filename selection in [`_all_filenames`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/commands/run.py).

### `additional_dependencies`

Upstream values are strings passed through to the language installer (`pip install`, `go install`, `cargo install`, `npm install`, ...). ahi keeps that string form so existing files still parse.

From v0.2, `ahi lock` refuses a dependency it cannot pin to a version plus one or more sha256 hashes, and the lockfile is the place those hashes live. `ahi run` never installs dependencies. A string that is only a floating range (`requests`, `ruff>=0.1`) is a lock error, not a run-time download.

### Policy file

`ahi.policy.yaml` sits next to `.pre-commit-config.yaml`. Upstream pre-commit does not read it. Absence means the defaults below, which are deny-by-default.

```yaml
version: 1
# Exact repo URLs allowed to be fetched. Empty denies every remote.
repo_allow: []
# Runtimes ahi may invoke. It will not download them.
runtime_allow: []
# Each item is an id plus the exact entry string. A bare id parses and does not authorize.
trusted_system_hooks: []
# Hook ids allowed to open the network. Default is none. Enforced once sandboxes exist.
# A trusted local hook is not sandboxed in this version, so the network is not blocked for it.
network_hooks: []
```

Unknown keys are errors. v0.1 parses the file when it is present and rejects a malformed one. v0.1 does not fetch, so `repo_allow` is not consulted yet. A `language: system` or `script` hook (stored as `unsupported` or `unsupported_script`) runs only when `trusted_system_hooks` contains that id and the exact `entry` string. `args` are not part of the pin. A bare id does not authorize.

### Filtering

Order, matching [`Classifier`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/commands/run.py):

1. Drop paths that do not exist (`os.path.lexists`). ahi uses symlink-aware existence and does not follow the link to decide existence.
2. Top-level `files` must match and top-level `exclude` must not (`re.search` semantics: unanchored search, not full match).
3. Hook `files` must match and hook `exclude` must not.
4. Type tags: `tags ⊇ types` AND (`types_or` is empty OR `tags ∩ types_or` is non-empty) AND `tags ∩ exclude_types` is empty.
5. `always_run: true` runs the hook even when the filtered set is empty.
6. `pass_filenames: false` runs the hook once with an empty filename list.

On Windows, upstream normalizes `\` to `/` before regex matching. ahi does that on every OS, so patterns authored with `/` behave the same on Windows, macOS, and WSL.

Regex dialect (**intentional divergence**): the Rust `regex` crate, which guarantees linear-time matching. Patterns that need Python-only features (lookahead, lookbehind, backreferences, conditional patterns) are a config error naming the pattern and the feature. They are not "best effort" matched. `(?i)`, `(?x)`, and `(?P<name>...)` are in the supported subset (the `regex` crate accepts the Python named-group spelling). This is a security choice: untrusted hook repos must not be able to hand ahi a regex that backtracks exponentially.

Type tags come from a vendored subset of [identify](https://github.com/pre-commit/identify) (MIT). v0.1 includes:

- Mode tags: `file`, `symlink`, `directory`, `socket`, `executable`, `non-executable`, `text`, `binary`.
- Filename and extension tags covering the languages in the inferred workload (Rust, Python, Go, TypeScript/JavaScript, shell, YAML, JSON, TOML, Dockerfile, Markdown, workflow YAML) plus the tags the builtin checks need.

A tag that identify would accept and ahi does not implement yet is a config error ("unrecognized type tag"), the same way upstream errors when identify does not know a tag. Full identify parity is a conformance target, not a v0.1 claim. The vendored table's source revision is recorded next to the table in the source file.

Text versus binary follows identify's `is_text` heuristic (NUL, or a non-text byte in the sample). **Unverified** against identify's current byte-for-byte sample size until the v0.1 tests pin the port. Executable versus non-executable prefers the git index mode (`git ls-files --stage`) when the path is tracked, because Windows working trees do not store the executable bit. That matches the Windows branch of `check-executables-have-shebangs` in [pre-commit-hooks](https://github.com/pre-commit/pre-commit-hooks/blob/main/.pre-commit-hooks.yaml). Untracked paths use the filesystem mode, except on Windows where an untracked file is `non-executable` unless a shebang is present. The shebang exception is a ahi rule so `types: [executable]` is not useless on native Windows; it is called out as a divergence.

### File discovery

| Mode | Selection |
| --- | --- |
| default | `git diff --staged --name-only --no-ext-diff -z --diff-filter=ACMRTUXB` (added, copied, modified, renamed, type-changed, unmerged, unknown, broken; not deleted) |
| `--all-files` | `git ls-files -z --deduplicate` |
| `--files a b` | the given paths, after the same existence and traversal checks |
| `--from-ref A --to-ref B` | `git diff --name-only --no-ext-diff -z A...B`, falling back to `A..B` if the three-dot form fails because there is no merge base |

Exactly one of these modes. `--from-ref` and `--to-ref` must be paired. During a merge with unmerged paths, default `ahi run` fails with "Unmerged files. Resolve before committing." rather than checking a mix of stages. That matches upstream `run()`.

Paths are repo-relative, UTF-8, and rejected when they are absolute, contain a NUL, contain a newline, or normalize outside the repository root. Symlink targets are not used as the path identity.

### Batching

Upstream partitions hook arguments in [`xargs.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/xargs.py) and shuffles filenames with a fixed Python `random.Random` seed (`1542676187`, version 1) in [`lang_base.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/lang_base.py) before partitioning. `require_serial: true` forces one worker. Otherwise the target concurrency is the CPU affinity count, or 1 when `PRE_COMMIT_NO_CONCURRENCY` is set.

The length budget is `SC_ARG_MAX - 2048 - environ_size`, clamped to `[2**12, 2**17]` on POSIX; `2**15 - 2048` on Windows; `2**12` elsewhere. Partition width is `max(4, ceil(n / target_concurrency))`. A single argument that does not fit is `ArgumentTooLongError`.

v0.1 implements this function and tests it. It does not reproduce CPython's MT19937 seeding; the shuffle is applied in v0.3 when there is a subprocess whose batch boundaries are observable. Builtin checks see the filtered list in path order.

### Rev, lockfile, and cache

Upstream caches clones in `PRE_COMMIT_HOME` or `$XDG_CACHE_HOME/pre-commit` or `~/.cache/pre-commit`, with a sqlite `db.db` keyed by `(repo, ref)` ([`store.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/store.py)). The ref is whatever string was configured. A tag and a branch are both just cache keys. Upstream warns when `rev` does not contain `.` and is not hexadecimal ([`WarnMutableRev`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/clientlib.py)), but it still clones.

ahi separates fetch from run.

`ahi.lock` (YAML, version 1), written by `ahi lock` / `ahi autoupdate` and verified by `ahi run` and `ahi verify` starting in v0.2:

```yaml
version: 1
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    requested: v6.0.0          # the rev string from the config, possibly a tag
    commit: <40 lowercase hex> # full commit that rev resolved to
    tree: <40 lowercase hex>   # git rev-parse <commit>^{tree}
    digest: sha256:<64 hex>    # sha256 of the canonical listing below
    dependencies:
      - id: <hook id>
        name: ruff
        version: 0.6.9
        artifacts:
          - arch: x86_64-unknown-linux-gnu
            digest: sha256:<64 hex>
```

Canonical listing for `digest`: the bytes of `git ls-tree -r -z <tree>` (mode, type, object id, tab, path, NUL), hashed with sha256. The git tree id is already content-addressed; the extra digest detects an ahi bug that compared the wrong object, and it does not require trusting only the tree id a mirror claims.

`ahi run` (v0.2+) resolves each URL repo through the lockfile, checks the cached objects against `commit`, `tree`, and `digest`, and does not open a network connection. A missing lockfile, a `requested` value that does not match the config, or a digest mismatch is an error. v0.1 does not read `ahi.lock` because it does not execute URL repos.

Cache directory, in order: `--cache-dir`, else `AHI_HOME`, else `$XDG_CACHE_HOME/ahi` when `XDG_CACHE_HOME` is set, else `std::env::temp_dir()/ahi` (this follows `TMPDIR`). There is no `HOME` or `USERPROFILE` fallback. Nothing in ahi assumes a writable home directory.

`Store::open` succeeds when `objects/sha256` already exists and a probe create in that directory fails. That is the read-only store baked into an image. `put` returns an error on a read-only store. `get` re-hashes and errors on mismatch. Writes, when the store is writable, go to a temporary file in the same directory, fsync, then rename. Hook scratch uses `std::env::temp_dir()` as well, so it stays under `/tmp` when that is the temporary directory.

Inside an OpenShell image the online step is the image build, which has network and which pins every download to an exact version and a per-architecture sha256. That bake is the same contract as `ahi fetch`. At runtime `ahi` reads the store and checks it against `ahi.lock`.

Layout, relative to the cache directory:

```
<cache>/
  README
  audit.log            # append-only JSON lines; a read-only store skips new lines
  objects/sha256/ab/cd...   # content-addressed blobs
  repos/<digest>/      # materialized tree for a locked repo
  stash/               # partial-commit patches, outside the worktree
```

### Partial commits

Upstream, for the default staged run only (`stash = not all_files and not files`), [`staged_files_only`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/staged_files_only.py):

1. Clears intent-to-add entries (`git rm --cached`, restored with `git add --intent-to-add` in `finally`).
2. Writes `git diff-index --ignore-submodules --binary` against `git write-tree` to a patch file under the store directory.
3. `git -c submodule.recurse=0 checkout -- .` so hooks see the index, not the unstaged worktree. Sets `_PRE_COMMIT_SKIP_POST_CHECKOUT=1` so the checkout does not re-enter hooks.
4. On the way out, `git apply --whitespace=nowarn`. On failure, retries with `core.autocrlf=false`. On a second failure, checks the index back out (discarding hook edits) and applies the patch again so the user's unstaged work returns.
5. If the diff command itself fails, it aborts. It does not continue with a dirty worktree.

ahi adopts that protocol before any builtin or hook may write a file, with these extra rules:

- The patch is checksummed and not deleted until apply has succeeded, or until the rollback apply has succeeded. A failed rollback is a fatal error that prints the patch path and does not exit 0.
- `--fix` is refused for a file that has unstaged changes. ahi does not stash, so it will not destroy that hunk and it also will not isolate the index.
- Read-only checks still only *report* on the filtered path list; they do not pretend to have isolated the index. A dirty unstaged hunk in a staged file can therefore affect a report. `ahi run` prints a one-line warning when the staged run sees unstaged changes. The stash isolation is still specified and not implemented. `ahi run --fix` writes only the three fixer checks, only paths in the selected set, atomically, and never through a symlink.

### Git hook shim

Upstream's generated hook is [`hook-tmpl`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/resources/hook-tmpl): a shell script that `exec`s `pre-commit hook-impl`. `hook-impl` translates each Git event into a `run` invocation ([`hook_impl.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/commands/hook_impl.py)), including pre-push's stdin of `local_ref local_sha remote_ref remote_sha`, the zero-sha delete case, and the "remote object missing" ancestry walk.

ahi's eventual shim calls `ahi hook-impl` with the same per-type argument counts. It is not installed in v0.1. The git 2.54 `git config set hook.<name>.event` form is documented by pre-commit as new in 4.6.0 ([docs](https://pre-commit.com/#usage-with-git-254-hook-configuration)). **Unverified:** this spec did not confirm against Git's own release notes that 2.54 is the version that introduced that config. ahi will not require that mechanism. The shell shim remains the portable install path.

`migrate-config` upstream rewrites three historical shapes ([`migrate_config.py`](https://github.com/pre-commit/pre-commit/blob/main/pre_commit/commands/migrate_config.py)): a top-level list becomes `repos:`, the key `sha` becomes `rev`, `language: python_venv` becomes `python`, and stages `commit` / `push` / `merge-commit` gain a `pre-` prefix. `ahi migrate` does not perform those rewrites. It is a best-effort reading aid: it prints which hook ids have an obvious `repo: builtin` equivalent, which `repo: local` system and script hooks can be carried over (with the policy snippet), and which hooks cannot be ported, with a reason. It does not guarantee feature parity or the same file scope. It prints the suggestion and leaves the file unchanged unless `--write` is set, and it refuses to replace an existing file unless `--force` is also set. The id table is not consulted by `ahi run`. A run-time mapping from remote `pre-commit-hooks` ids onto builtins was removed; remote hooks keep failing with the explicit not-executed error. Legacy stage names stay a parse error.

## Threat model

Assets: source trees, credentials (including provider placeholders), the ahi cache, and the git index (including unstaged work that must survive a hook run).

Adversaries:

1. A hook repository named in `.pre-commit-config.yaml` (compromised maintainer, or a repo the developer did not mean to trust).
2. A config or pull request that changes `rev`, `entry`, `additional_dependencies`, or a workflow file.
3. A file in the tree under check: path traversal, Trojan Source, a secret about to be committed, a symlink pointing outside the repo.
4. A local process racing the cache or the stash patch.

Out of scope: a kernel exploit, and a person who already controls the account that runs ahi. Inside an OpenShell sandbox that person can edit hooks and config, so a hook run there is advisory fast feedback. The enforcement backstop is CI, which runs `ahi` from the trusted workflow on the commit under review. Host-side commit import and host-side signing are not part of this deployment. Git clone, commit, pull, and push happen inside the sandbox. When ahi is the component applying a sandbox, that sandbox contains hook subprocesses. It is not a claim that the ahi binary is isolated from the user who invoked it. Hook code is not trusted.

Trust boundaries:

| Boundary | Rule |
| --- | --- |
| Config | Untrusted data. Parsed with limits (file size cap 1 MiB, depth cap, no YAML aliases/anchors — anchors are rejected so a config cannot expand into a billion nodes). Inside the sandbox the same file is editable by the sandbox user, so the run is advisory and CI is the backstop. |
| `rev` | Not a branch. v0.2 stores the full commit and a tree digest. Tags are resolved only by `ahi lock` / `ahi autoupdate`, which are online on purpose. |
| Cache | Content-addressed. Run verifies before use. The store opens read-only when the object directory cannot be written. The cache path is not inside the repository being checked, and it is not assumed to live under a home directory. |
| Network | `ahi fetch` and `ahi lock` / `ahi autoupdate` may use the network. Builtin checks do not open one. A trusted local hook is not sandboxed in this version, so ahi cannot stop that process from opening a socket. |
| Runtimes | A name on `runtime_allow` plus a binary already on `PATH` or an explicit path. No nodeenv, no rustup installer, no Go tarball. |
| `system` / `script` | Denied unless `trusted_system_hooks` has the hook id and the exact `entry`. A bare id does not authorize. |
| Network-using hooks | `network_hooks` is the allowlist once a sandbox can block the network. This version has no sandbox, so it does not pretend the allowlist is enforced. |
| Environment | Local system and script hooks get the scrubbed environment below. |
| Files the check reads | Opened with `O_NOFOLLOW` where the OS supports it. Paths that escape the repo root are findings, not inputs. |

## Security behavior

### Revisions and the lockfile

Specified in [Rev, lockfile, and cache](#rev-lockfile-and-cache). `ahi autoupdate` moves `requested` to a tag only by writing both the tag and the resolved commit into the lockfile. It does not replace a full SHA in the config with a moving branch. `--bleeding-edge` is rejected: ahi will not record `rev` as a branch tip. Upstream's `--freeze` (store the SHA in the config, with a comment naming the tag) is the model for what `ahi autoupdate` writes into the lockfile; the config may keep the tag so diffs stay readable.

### Allowlist and offline run

`repo_allow` is an exact string match against the `repo` value (no implicit trailing `.git` equivalence in v0.2; `https://github.com/org/repo` and `https://github.com/org/repo.git` are different entries). **Recommendation, open to Ian:** also accept a single canonicalization (strip a trailing `.git`, lowercase the host) so the allowlist is not trivially bypassed by spelling. v0.2 should implement that canonicalization and say so in the lockfile.

`ahi fetch` is the command that fills the cache. After it succeeds, `ahi run` works with the network namespace unavailable. A test in v0.2 runs `ahi run` with the network blocked.

### Runtimes

v0.3 languages that need a runtime:

| Language | Runtime | Install command ahi will use | Will not do |
| --- | --- | --- | --- |
| `python` | an allowed `python` or `uv` already installed | `uv` sync/install with `--require-hashes` against the locked hashes | `virtualenv` bootstrap that downloads pip, or pip without hashes |
| `rust` | an allowed `cargo` already installed | `cargo install --locked --bins` | download `rustup-init` |
| `golang` | an allowed `go` already installed | `go install` at the locked module version, `GOSUMDB` on, checksum database verified | download a Go tarball |
| `docker_image` | an allowed `docker` or `podman` | run the image already present locally and named by digest | `docker pull` during `run` |

`language_version: default` means "the allowed binary", not "download this version". A `language_version` other than `default` is an error unless that exact version is the one on `PATH` (`python3.12 --version`, `go version`, `rustc --version`). ahi does not satisfy a mismatch by installing.

Node is not in this table. See [Open decisions](#open-decisions).

### `additional_dependencies`

Each locked artifact has a version and at least one sha256. `ahi fetch` checks the hash before the file is marked present. A mismatch deletes the temp file and errors. There is no "continue anyway".

### `system` / `script`, network, environment

A local system or script hook runs only with an exact-entry pin, as described under the policy file. There is no sandbox in this version, so the network is not blocked for that process. The environment passed to the hook is:

- Kept, if set: `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `TMPDIR`, `TEMP`, `TMP`, `SYSTEMROOT`, `WINDIR`, `COMSPEC`, `PATHEXT`, `TERM`.
- Dropped: everything else, including `GITHUB_TOKEN`, `GH_TOKEN`, `AWS_*`, `AZURE_*`, `NPM_TOKEN`, `CARGO_REGISTRY_TOKEN`, `SSH_AUTH_SOCK`, and any variable whose name contains `TOKEN`, `SECRET`, `PASSWORD`, or `KEY`.

`SSH_AUTH_SOCK` is dropped on purpose. Upstream documents it as required for `git@` clones ([tox section](https://pre-commit.com/#usage-with-tox)). ahi cloning happens in `ahi fetch`, which is allowed a separate, unscrubbed environment. Hook code does not get the agent socket.

`PRE_COMMIT=1` is set, matching upstream since 2.5.0, so hooks that branch on it keep working.

`CARGO_HOME` and `RUSTUP_HOME` are not on the keep list. A rustup proxy that needs them cannot select a toolchain. A toolchain installed under the default `$HOME/.rustup` still works because `HOME` is kept.

### Sandboxes (v0.4)

v0.4 first detects that ahi is already inside an OpenShell sandbox and does not nest Landlock or seccomp. The OpenShell runtime profile denies `seccomp(SECCOMP_SET_MODE_FILTER)` with `EPERM`, so a nested filter is expected to fail. Network control in that environment is the policy proxy, not a second sandbox. **Unverified:** nesting was not attempted, and OpenShell-on-macOS and OpenShell-on-native-Windows were not described by the Linux docs that were fetched.

Host-side sandboxing is an option for a process that is not already sandboxed, applied to hook subprocesses and not to builtin in-process checks (those are ahi code). The option is lower priority than the detect-and-do-not-nest path.

| OS | Backend | What it can actually do | Limit, stated honestly |
| --- | --- | --- | --- |
| Linux | Landlock for filesystem read/write allowlists, plus a seccomp filter on a fresh child for network sockets | Hide the rest of the disk; block `connect` when the kernel allows the filter | Landlock network rules need a new enough ABI. A thread-heavy parent cannot apply seccomp to itself safely; only children started after the filter. User namespaces are not required for this path. Nesting this inside OpenShell is not the plan. |
| Linux | `bwrap` if Landlock is unavailable | Mount namespace with only the repo and the cache bind-mounted | Often unavailable in containers (`user.max_user_namespaces=0`). ahi then refuses networked or `system` hooks rather than pretending. |
| macOS | `sandbox-exec` with a generated SBPL profile `(deny default)` plus explicit allows | Best-effort filesystem and network denial for the child | Apple has treated `sandbox-exec` as a deprecated, undocumented interface for years. It is not a security boundary against a process that can exploit the kernel, and profile behavior differs across OS versions. ahi reports the backend name in `ahi doctor` and does not claim parity with Landlock. **Unverified:** no sandbox profile was executed while writing this spec. |
| Windows | A job object (kill-on-close, active-process limit) around the child | Contains the process tree so a hook cannot orphan itself | A job object does not restrict filesystem or network. Restricted tokens / AppContainer are the real filesystem boundary and are not promised for the first Windows sandbox cut. `ahi doctor` will say "process-tree only" on Windows until that exists. |

If ahi itself selected a backend, and that backend cannot enforce "no network", and the hook is not in `network_hooks`, the hook does not run.

Builtin checks stay in-process and offline. They do not need a sandbox to avoid the network; they never open one.

### Signatures, audit, path safety

- **Signatures on fetched hook repos.** A lower-priority v0.2 field, not the way Ian produces commits. Where a fetched hook commit carries a gitsign or sigstore signature, `ahi fetch` may verify it and record `signature: verified | absent | failed | unverified` in the lockfile. `failed` is a hard error. `absent` is allowed and reported by `ahi verify`, because most hook repos are not signed today. A missing verifier is `unverified`, and `ahi verify --require-signatures` fails closed. ahi does not import or sign the user's commit on the host. **Unverified:** no gitsign bundle was validated while writing this spec.
- **Audit log.** Append-only JSON lines in the cache directory's `audit.log`: time, command, repo root, hook id, outcome, finding count. No file contents, no environment, no tokens. v0.1 writes one record per `ahi run` when the cache is writable. A read-only cache skips the record with a warning and does not fail the run.
- **Path traversal.** Filenames from git and from `--files` are normalized and must stay inside the repo root. A symlink whose target escapes the root is a finding of `ahi/symlinks`, and content checks do not follow it.
- **Filename safety.** NUL and newline in a path are rejected. When subprocesses exist, filenames are passed after `--` and never through a shell. The v0.1 runner does not invoke a shell for builtins.
- **Stash.** The protocol in [Partial commits](#partial-commits). It is not implemented. `ahi run --fix` can write a selected source file for the three fixer checks, and it refuses that write when the file has unstaged changes.

## Builtin check catalog

Builtins are offline, in-process, and selected with `repo: builtin`. The canonical id is `ahi/<name>`. Aliases exist so a short name also resolves. Defaults for `types` apply when the config hook does not set `types`.

Checks report findings. `ahi/end-of-file`, `ahi/trailing-whitespace`, and `ahi/line-endings` rewrite a selected file only when `ahi run --fix` or a hook argument `--fix` is set, then fail the hook because the file changed. Without that flag they only report. Other checks, including `ahi/byte-order-marker`, never write.

Severity is `error` unless noted. A single error finding fails the hook.

### v0.1 set

| Id | Alias | Default types | Behavior |
| --- | --- | --- | --- |
| `ahi/trailing-whitespace` | `trailing-whitespace` | `text` | A line with trailing whitespace. Markdown (`.md`, `.markdown`, and any extra extensions passed as `args: [--markdown-linebreak-ext=md,markdown]`) may keep a trailing double space on a non-blank line, matching [`trailing_whitespace_fixer.py`](https://github.com/pre-commit/pre-commit-hooks/blob/main/pre_commit_hooks/trailing_whitespace_fixer.py). `--chars` is accepted as an argument. Report-only unless `--fix`. |
| `ahi/end-of-file` | `end-of-file-fixer` | `text` | Non-empty file whose last line ending is missing, or that has more than one trailing newline sequence. Empty files are fine. Logic matches the cases in [`end_of_file_fixer.py`](https://github.com/pre-commit/pre-commit-hooks/blob/main/pre_commit_hooks/end_of_file_fixer.py). Report-only unless `--fix`. |
| `ahi/merge-conflict` | `check-merge-conflict` | `text` | A line starting with `<<<<<<< `, `=======` (including the newline forms), or `>>>>>>> `. Upstream returns success when git is not in a merge ([source](https://github.com/pre-commit/pre-commit-hooks/blob/main/pre_commit_hooks/check_merge_conflict.py)). ahi always scans. Conflict markers are worth failing on after the merge is over. |
| `ahi/large-files` | `check-added-large-files` | `file` | `ceil(bytes / 1024) > max_kb`. Default `max_kb` is 500, matching upstream. Argument `--maxkb=N`. Upstream by default only checks newly added files unless `--enforce-all`. ahi checks every selected file (the `--enforce-all` behavior), because "added" is surprising in CI. Git LFS skip is not in v0.1; a pointer file is small anyway. |
| `ahi/case-conflict` | `check-case-conflict` | `file` | Two tracked or selected paths that differ only by case, including parent directories. Algorithm matches [`check_case_conflict.py`](https://github.com/pre-commit/pre-commit-hooks/blob/main/pre_commit_hooks/check_case_conflict.py). |
| `ahi/symlinks` | `check-symlinks` | `symlink` | Broken symlink (upstream), and a symlink whose absolute target escapes the repository root (ahi). Escaping `..` chains are resolved lexically against the link's directory without requiring the target to exist. |
| `ahi/check-json` | `check-json` | `json` | JSON parse failure, including duplicate object keys. |
| `ahi/check-yaml` | `check-yaml` | `yaml` | YAML parse failure. Anchors and aliases are accepted in files under check (they are user data, not ahi config) up to a node budget. Multiple documents are accepted. |
| `ahi/check-toml` | `check-toml` | `toml` | TOML parse failure. |
| `ahi/trojan-source` | none | `text` | Unicode bidi controls from the Trojan Source paper: U+202A, U+202B, U+202C, U+202D, U+202E, U+2066, U+2067, U+2068, U+2069. Also a documented homoglyph set: Cyrillic and Greek letters that share a glyph with Latin, when the same line also contains a Latin letter. This is not the full Unicode confusables database. Source of the bidi set: Boucher & Anderson, "Trojan Source", 32nd USENIX Security (the paper; **unverified** against a local PDF — the code point list is the widely cited one and is pinned by tests). |
| `ahi/windows-reserved-names` | `check-illegal-windows-names` | `file` | A path component equal, case-insensitively, to `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, including the superscript `¹` `²` `³` forms upstream mentions, or a component containing `<>:"\|?*` or a trailing space or dot. Upstream expresses this as one `language: fail` regex in [`.pre-commit-hooks.yaml`](https://github.com/pre-commit/pre-commit-hooks/blob/main/.pre-commit-hooks.yaml). ahi implements it as a path check so the message names the component. |
| `ahi/private-key` | `detect-private-key` | `text` | File bytes contain one of the header strings in upstream `detect_private_key.py`. The list is copied into the source with a comment pointing at that file's revision. v0.1 does not add a general entropy scanner; that false-positive class is left out on purpose. |
| `ahi/github-actions` | none | workflow files (`*.yml` / `*.yaml` under `.github/workflows/`) | See below. |
| `ahi/dockerfile` | none | `dockerfile` | See below. |
| `ahi/shell-strict` | none | `shell` | See below. |
| `ahi/committed-dotenv` | none | `file` | A tracked or selected file whose base name is `.env`, or `.env.*`, except `.env.example`, `.env.sample`, and `.env.template`. Does not flag `versions/*.env` (those are pin files, not dotenv files). |
| `ahi/line-endings` | `mixed-line-ending` | `text` | Mixed CR, LF, and CRLF in one file. Default is report-only, like upstream `--fix=no`. `ahi run --fix` rewrites with `--fix=auto` (LF wins a tie). A hook argument `--fix=lf`, `--fix=crlf`, `--fix=cr`, or `--fix=auto` rewrites. `--fix=no` stays report-only. |
| `ahi/shebang-executable` | `check-executables-have-shebangs` | `text`, `executable` | An executable text file with no `#!` shebang. Uses the git index mode. |
| `ahi/executable-shebang` | `check-shebang-scripts-are-executable` | `text` | A tracked text file with a shebang that is not executable in the git index. Untracked files are skipped. |
| `ahi/no-commit-to-branch` | `no-commit-to-branch` | (none; `always_run` defaults to true and `pass_filenames` to false) | The current branch is `main` or `master`, unless `--branch` / `-b` names a different set. `--pattern` / `-p` is start-anchored. A detached HEAD passes. |
| `ahi/check-xml` | `check-xml` | `xml` | Not well-formed XML (unclosed elements, multiple roots, missing root). A doctype with `SYSTEM`, `PUBLIC`, or `ENTITY` is refused. External entities are not resolved and no network request is made. Parsed with quick-xml 0.41. |
| `ahi/aws-credentials` | `detect-aws-credentials` | `text` | An access key id (`AKIA`, `ASIA`, `ABIA`, or `ACCA` plus 16 uppercase letters or digits) or a 40-character secret, session, or security token assigned on the same line. Scans the selected files only. Does not read `~/.aws`, a credentials file, or the environment. `--credentials-file` is refused. This is narrower than upstream, which loads secrets from those files and searches for the exact bytes. |
| `ahi/vcs-permalinks` | `check-vcs-permalinks` | `text` | A GitHub blob link whose revision is not 4 to 64 hex digits and that points at a line number. A 40-hex permalink is allowed. `--additional-github-domain` adds a host. |
| `ahi/byte-order-marker` | `fix-byte-order-marker` | `text` | A leading UTF-8 BOM (`EF BB BF`). Report-only. `--fix` does not strip it. |

#### `ahi/github-actions`

Applies to workflow files only.

1. Every `uses:` value is one of: `owner/repo@` plus 40 lowercase or uppercase hex digits; `owner/repo/subpath@` plus 40 hex digits; `docker://image@sha256:` plus 64 hex digits; or a relative `./` action. Tags (`@v4`), branches (`@main`), and short SHAs are errors.
2. The workflow has a top-level `permissions` mapping, or every job has one. Missing permissions is an error. `write-all` is an error. A permission value other than `read` or `none` is an error. An empty mapping is allowed (that is "none").
3. `pull_request_target` is an error when a checkout step sets `ref` from the pull request head (`pull_request.head`, `head.sha`, `head.ref`), or when any `run:` step under that trigger interpolates `secrets`. A `pull_request_target` workflow that only labels from trusted base-repo code, with read or empty permissions and no head checkout, is allowed.

These rules are stricter than GitHub's defaults. A release workflow that needs `contents: write` fails until the hook args allow it: `args: ["--allow-write=contents"]`. The allowlist is per permission name, not a global off switch.

#### `ahi/dockerfile`

1. Every `FROM` image is pinned with `@sha256:` and 64 hex digits. `scratch` is the exception. Stage names (`AS builder`) are ignored for the pin check. The pin is required on every stage, not only the last.
2. The final stage has a `USER` instruction whose value is not `root`, `0`, or `0:0`. A missing `USER` is an error.
3. A `RUN` line that pipes `curl` or `wget` into `sh`, `bash`, or `sudo sh` / `sudo bash` is an error. This is the `curl | sh` pattern, including simple wraps.
4. A `RUN` that invokes `curl` or `wget` to fetch a URL is an error unless that same `RUN` also contains a checksum check (`sha256sum`, `sha384sum`, `sha512sum`, `shasum`, or `cosign verify`). **This is a heuristic.** It misses a download in one layer verified in a later layer, and it will say so in the finding text. The later-layer case is a v0.2 refinement, not a silent pass.
5. `apt-get install` / `apt install` package arguments that lack `=version` are errors, except package names that are only flags (`-y`, `--no-install-recommends`).

#### `ahi/shell-strict`

- Bash (shebang contains `bash`, or the file name ends in `.bash`): the file must contain `set -euo pipefail`, or all three of `set -o errexit`, `set -o nounset`, and `set -o pipefail` (separate `set` lines count).
- POSIX `sh` (shebang ends in `sh` and not `bash`): `set -eu` or the `errexit` and `nounset` options. `pipefail` is not required, because it is not POSIX. The finding text says that.
- A `.sh` file with no shebang is treated as POSIX `sh`.

### Later catalog entries (not v0.1)

Specified so the ids stay stable. Each returns "not implemented" if somehow selected before its phase, rather than passing.

| Id | Intent |
| --- | --- |
| `ahi/file-mode` | A text file tracked as executable without a shebang, or a mode outside `100644` / `100755`. Cross-platform normalization so WSL and Windows agree. |
| `ahi/version-pins` | Files under `versions/*.env` (or a configured directory) contain `NAME_VERSION=...` and `NAME_SHA256_<ARCH>=<64 hex>` for each architecture the file claims. A version key with no checksum, or a checksum that is not 64 hex digits, is an error. The directory default is `versions`. |
| `ahi/secrets` | A second, narrower pass beyond private-key headers (cloud tokens with a known prefix). Not an entropy scanner. |

## CLI

Binary name: `ahi`. Global flags, matching the upstream shape where it exists:

- `--version` prints `ahi <version>` and nothing else. `<version>` is `env!("CARGO_PKG_VERSION")`. The smoke test compares the process output to that string.
- `--color auto|always|never` (default `auto`). v0.1 reads the flag. An `AHI_COLOR` / `TERM=dumb` override, in the same role as `PRE_COMMIT_COLOR`, is specified and not read yet.
- `-c, --config <PATH>` selects a config file. Default discovery is `.pre-commit-config.yaml` then `.pre-commit-config.yml` in the git root.
- `--cache-dir <PATH>` selects the cache. It wins over `AHI_HOME`. See [Rev, lockfile, and cache](#rev-lockfile-and-cache).
- `-h, --help`.

| Command | Phase | Behavior |
| --- | --- | --- |
| `ahi run [HOOK]` | v0.1 | Run selected hooks. Flags: `--all-files`, `--files <PATH>...`, `--from-ref`, `--to-ref`, `--hook-stage`, `--format text\|json\|sarif`, `--fail-fast`, `--verbose`, `--fix`. |
| `ahi list-checks` | v0.1 | Builtin ids, aliases, one-line descriptions. `--format text\|json`. |
| `ahi explain <ID>` | v0.1 | Description, defaults, arguments, and the upstream hook it corresponds to, if any. |
| `ahi install` / `uninstall` | shim phase (after v0.3, before relying on it in git) | v0.1 exits 1 with "not implemented until the hook shim phase". |
| `ahi lock` / `verify` / `fetch` / `autoupdate` | v0.2 | v0.1 exits 1 naming v0.2. |
| `ahi doctor` | v0.1 prints version, git version, config path, cache path, whether that cache is writable or read-only, the scratch directory, and `sandbox: not probed in this version` | v0.4 reports whether an outer sandbox was detected and which host backend, if any, would be used. |
| `ahi migrate` | v0.1 | Best-effort suggestion. Prints a report and a suggested config. Does not guarantee feature parity or the same file scope. Writes only with `--write`, and refuses to replace an existing file unless `--force` is set. Not upstream `migrate-config`. |
| `ahi hook-impl` | with install | Hidden. Not in v0.1. |

`ahi run` text format follows the upstream rhythm: a hook title, then `Passed`, `Failed`, or `Skipped`, then indented findings `path:line: message` when a line is known.

JSON format (stdout, one document):

```json
{
  "ahi_version": "0.1.0",
  "result": "failed",
  "hooks": [
    {
      "id": "ahi/trailing-whitespace",
      "outcome": "failed",
      "findings": [
        {
          "path": "src/lib.rs",
          "line": 12,
          "column": 1,
          "severity": "error",
          "message": "trailing whitespace"
        }
      ]
    }
  ]
}
```

SARIF is version 2.1.0. One run, one driver named `ahi`, one rule per builtin id, results with `ruleId` and `physicalLocation`. No SARIF extensions that would require a second tool to read the file.

Unsupported-language failure text, stable enough to test:

```
hook `<id>` uses language `<language>`, which is not supported until v0.3
```

Languages outside the v0.3 plan say `not supported by ahi` instead of a version number, so the message does not promise Node later.

## Crate and module layout

One package, edition 2021, MSRV in `rust-version`. The package name, `[lib] name`, and `[[bin]] name` are all `ahi`, so `cargo package` and `cargo publish` operate on the root manifest the way a single-crate release script expects. `fuzz/` is a detached workspace and is not part of that package's publish.

```
Cargo.toml                  # package ahi, lib + bin
src/                        # library modules and src/main.rs
tests/                      # CLI and golden tests
fuzz/                       # detached libFuzzer target
deny.toml
justfile
.github/workflows/ci.yml
.github/workflows/publish.yml
scripts/cut-release.sh
```

Library modules:

| Module | Responsibility |
| --- | --- |
| `ahi::config` | Parse and validate `.pre-commit-config.yaml` and `.pre-commit-hooks.yaml`. Errors carry path, line, and column when the parser provides them. |
| `ahi::policy` | Parse `ahi.policy.yaml`. |
| `ahi::identify` | File-type tags. |
| `ahi::filter` | Include/exclude/types and the xargs partition helper. |
| `ahi::git` | Staged / all / range file lists, repo root, file mode. |
| `ahi::check` | The `Check` trait, the builtin registry, and one submodule per check. |
| `ahi::runner` | Select hooks for a stage, filter files, apply `SKIP` and `fail_fast`, collect a report. |
| `ahi::store` | Content-addressed put/get/verify. No network client in this module. |
| `ahi::output` | Text, JSON, SARIF. |
| `ahi::audit` | Append a JSON line to the audit log. |

Public trait, kept small:

```rust
pub trait Check: Send + Sync {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn default_types(&self) -> &'static [&'static str];
    fn check(&self, ctx: &CheckContext<'_>, files: &[CheckedPath<'_>]) -> CheckOutcome;
}
```

`CheckContext` carries the repository root, parsed args, and a file opener that refuses symlink follows and paths outside the root. Builtins do not take an environment map and do not receive a `std::process::Command` handle. That split is what keeps "native checks cannot pull Node" true by construction.

The binary is the same package as the library. It contains argument parsing and process exit codes, and it calls the library.

### Dependencies

Each direct dependency is justified in the v0.1 pull request. The allowed reasons are: CLI parsing, YAML/JSON/TOML parsing, safe regular expressions, typed errors, and (dev) property tests. HTTP clients, git library wrappers, Node, Python, and sandbox crates are not v0.1 dependencies. SARIF is emitted with `serde_json`, not a SARIF crate.

Transitive dependencies must pass `cargo deny`. The allow list in `deny.toml` is MIT, Apache-2.0, BSD-3-Clause, Unicode-3.0, and Zlib. crates.io is the only source. Yanked crates are denied. Advisories fail CI, with the one written exception for the unmaintained `unsafe-libyaml` advisory recorded in `deny.toml` (the maintained YAML crates need a newer Rust than the MSRV).

## Testing strategy

- Unit tests live next to the parser, filter, identify, and each check.
- Property tests (`proptest`) generate filename lists, regex subsets drawn from the supported dialect, and type-tag sets. The oracle is the logical definition in [Filtering](#filtering): AND of includes, NOT of excludes, superset `types`, intersecting `types_or`, disjoint `exclude_types`. Partition tests assert every argument appears once, order is preserved within a batch, no batch exceeds the length budget, and a too-long single argument is an error.
- Parser fuzzing: `proptest` feeds arbitrary strings and asserts the parser returns `Ok` or `Err` and does not panic. A `cargo-fuzz` target under `fuzz/` covers the same entry point for manual libFuzzer runs. The libFuzzer target is not required to build on the Windows CI job (it needs a sanitizer toolchain). The proptest is required on every OS.
- Golden tests: `tests/golden/` holds text, JSON, and SARIF snapshots for `ahi run --all-files` against a fixture built in the CLI test.
- Conformance: table-driven cases taken from the upstream functions cited above (empty `files`, `exclude: ^$`, type AND/OR, `--diff-filter`, merge-conflict patterns, large-file `ceil` boundary, markdown double-space). Where `pre-commit` and `identify-cli` happen to be installed they may be used as an optional oracle; CI does not depend on them. **Unverified:** a live `pre-commit` binary was not used as an oracle.
- Smoke: after `cargo build`, run `ahi --version` and require the output to equal `ahi ` plus the version in the root `Cargo.toml`. Exact match, not a prefix. `scripts/smoke-version.sh` accepts either `target/release/ahi` or `target/debug/ahi`.
- `cargo deny check` and `cargo audit` in CI, on Linux, using the pinned release binaries from `scripts/install-supply-chain-tools.sh`. `cargo audit` needs the network and the advisory database; if the database cannot be fetched, that job fails rather than skipping.

## Release and CI plan

The scripts and workflows are in the tree. This work does not create a tag, does not create a GitHub Release, does not run `cargo publish` (including a dry run against the real registry), and does not run `scripts/cut-release.sh`.

Versioning follows the `rfconversions` tag `v0.7.7` shape unless the open SemVer-versus-calver decision says otherwise: `scripts/cut-release.sh` takes an explicit `--version`, checks a SemVer pattern, tags `vX.Y.Z`, and the current cargo version is `0.1.0`. The script does not infer the next version. See [Open decisions](#open-decisions) and [docs/release.md](release.md).

`CARGO_REGISTRY_TOKEN` is a repository secret and would publish for real if the publish job ran. The workflows keep it unreachable from a branch push, a pull request, or a fork:

- `.github/workflows/ci.yml` triggers on `push` to `main` and on `pull_request` targeting `main`. It does not trigger on tags. It does not mention `CARGO_REGISTRY_TOKEN`. It does not run `cargo publish`. `cargo package` only checks the local crate. The file does not use `pull_request_target`, so a fork pull request is not handed this repository's secrets.
- `.github/workflows/publish.yml` is a separate file. Its only trigger is `release` with type `published`. The token is mapped in one step, on the `publish` job, and nowhere else. That job also requires `github.event_name == 'release'`, `github.event.action == 'published'`, a ref under `refs/tags/`, and `github.repository == 'iancleary/ahi'`. The `ci` job in that file re-runs the tests and does not receive the token. `permissions` stay `contents: read` at the workflow and on the publish job. Publishing to crates.io does not need `contents: write`. Creating the GitHub Release is the local script, not this workflow.
- A pull request cannot start `publish.yml`, so editing that file on a branch does not execute it. A fork does not receive the upstream secret. A release published from any other repository name does not pass the job condition.

Other CI facts:

- Actions are pinned to full commit SHAs, with a comment naming the upstream ref they came from. `actions/checkout` is `3d3c42e5aac5ba805825da76410c181273ba90b1` (the commit behind tag `v7` on 2026-10-25). `dtolnay/rust-toolchain` is `683b588c487a69d01f6c6ddf40f036ff682cc2c8` (master on 2026-10-16) and installs Rust 1.83.0 plus rustfmt and clippy.
- The matrix is `ubuntu-latest`, `macos-latest`, and `windows-latest`. Formatting, clippy (`-D warnings`), tests, `cargo doc` with warnings denied, `cargo package`, and `cargo build --release` run on every OS. cargo-deny and cargo-audit run on Linux only, because the advisory database does not need to be fetched three times. The version smoke runs on every OS.
- MSRV is 1.83 because that is the compiler used to verify v0.1 (rustc 1.83.0, 2024-11-26). CI installs that toolchain on purpose.
- `permissions: contents: read` is set at workflow level. No job widens it.
- The `justfile` recipes match the `rfconversions` names: `help`, `fmt`, `format`, `fmt-check`, `lint`, `lint-fix`, `test`, `doc-check`, `package`, `build`, `check`, `ci`, `cut-release`, `dev`. The `ci` recipe also runs the supply-chain installer, `cargo deny`, `cargo audit`, and the version smoke, which the workflow runs and which the `rfconversions` `ci` recipe does not.
- Deviations from the `rfconversions` workflow, beyond pinning SHAs, least privilege, the matrix, and the tag gate: the toolchain is 1.83.0 via the pinned dtolnay action rather than `rustup update stable`; deny and audit are Linux-only and use sha256-pinned upstream binaries rather than `cargo install`; publish lives in its own workflow file so the token is not in the pull-request workflow; the publish job's `if` requires a published release, a tag ref, and `github.repository == 'iancleary/ahi'` rather than `github.ref_type == 'tag'` alone; a version smoke step is added; MSRV is 1.83 rather than 1.70.
- Non-root: the workflow does not use a root container, and tests do not require root.

## Roadmap

### v0.1 — this implementation slice

Config and manifest parsing, policy-file parsing, git file discovery, filtering, identify subset, builtin checks in the v0.1 table, content-addressed store primitives (no network), text/JSON/SARIF, audit line, `ahi run`, `ahi list-checks`, `ahi explain`, `ahi doctor` (informational), `ahi migrate` (best-effort, no parity guarantee), stubs for the other commands, tests, `justfile`, `cargo deny`, CI matrix, README.

A URL or `meta` hook fails the run with an explicit error. It is not skipped and not substituted. `repo: local` with `language: system` or `script` runs when the policy pins the id and the exact entry. Other local languages fail the hook. `ahi run --fix` rewrites the three whitespace checks without the stash protocol; a file with unstaged changes is refused. Network is not blocked for a trusted local hook, because v0.4 is the sandbox. The environment is scrubbed to the keep list in this spec (`CARGO_HOME` and `RUSTUP_HOME` are not on that list).

### v0.2 — lock and fetch

`ahi.lock` as specified. `ahi lock`, `ahi verify`, `ahi fetch`, `ahi autoupdate`. Tag resolution happens here and only here. The image-build bake is this command's contract: exact version, per-architecture sha256, then a read-only store. Runtime network tests: `ahi run` with networking disabled after a populate. `repo_allow` enforced. A signature field on fetched hook repos is recorded when a verifier is available. Still no hook subprocesses. Loading config and the lockfile from a git ref, and refusing a config modified by the checked range, stay lower priority and are not required for v0.2.

### v0.3 — limited languages

Execute `fail`, `pygrep`, `python` (uv, hashes), `rust` (`cargo --locked`), `golang` (system Go, checksums), and `docker_image` (digest already local). `language: system` / `script` and the three `--fix` checks were pulled forward; this phase still adds the other languages. No Node, no runtime downloads. The stash protocol, with a test that an unstaged hunk survives a failed and a successful fix, is still this phase's contract. v0.1 refuses the unstaged file instead.

### v0.4 — detect an outer sandbox, and offer host sandboxing

Detect that ahi is already sandboxed and do not nest. `ahi doctor` reports that. Host-side backends in the sandbox table are an option, with the limits written there. A Linux test for the host option checks that a fixture hook's blocked read or connect is denied. macOS and Windows tests assert the documented weaker guarantee. OpenShell on macOS or native Windows is unverified until those docs or a run exist.

## Open decisions

The crate name is decided. The other items are not. Each open item has a recommendation. v0.1 implements the recommendation only where a choice is required to compile, and it names that choice.

### 1. License

The tree already contains an MIT license. prek, lefthook, and hk are MIT as well.

**Recommendation:** dual MIT OR Apache-2.0. That is the usual Rust crate pairing, and Apache-2.0 adds an explicit patent grant that a security tool benefits from. Keep MIT so the existing grant stays.

**Provisional, because the file is already in the repo:** MIT only, unchanged. v0.1 does not add `LICENSE-APACHE` and does not relicense. If Ian picks dual, add `LICENSE-APACHE` and switch the crate `license` field to `MIT OR Apache-2.0`. If Ian picks Apache-2.0 only, replace MIT; that is a larger change and should not be done quietly.

### 2. Crate name

**Decided: `ahi`.** The package, the library target, and the binary are `ahi`. Ian floated "agent hook interface" as an expansion. That expansion is not confirmed.

Checked on 2026-10-25, before this name was written into `Cargo.toml`:

| Registry | Result |
| --- | --- |
| crates.io `GET /api/v1/crates/ahi` | Free. HTTP 404, body `crate ahi does not exist`. |
| npm `registry.npmjs.org/ahi` | Taken. 0.1.0, an HTML5 canvas game engine by Geoff Blair, license ISC. |
| PyPI `/pypi/ahi/json` | Taken. 0.19.47, summary "Application Hacking Interface. An HTTP API client helper for unofficial APIs." |
| RubyGems `api/v1/gems/ahi.json` | Free. HTTP 404. |
| NuGet `v3-flatcontainer/ahi/index.json` | Free. HTTP 404. |
| Homebrew formula and cask `ahi.json` | Free. HTTP 404. |
| AUR RPC v5, `arg=ahi` | Free. `resultcount` 0. |
| Arch Linux package search `name=ahi` | Free. Empty results. |
| `proxy.golang.org/ahi/@v/list` and `pkg.go.dev/ahi` | Free as a module path. HTTP 404. |

Names can be squatted after this check. Re-check crates.io before any publish. This work does not publish.

### 3. How strict versus how compatible

**Recommendation:** fail closed on the security boundary (unknown keys, mutable or non-SHA revs once locking exists, regex features that can backtrack, system/script, missing workflow permissions, Dockerfile without a digest). Stay compatible on file discovery, the filtering algebra, stage names, and the `SKIP` variable. Do not add a `--compat=pre-commit` flag in v0.1; if a real config in Ian's repos cannot move, add the flag later for the specific warning-to-error cases (unknown keys), never for the sandbox rules.

Legacy stage names are rejected rather than rewritten in place. `ahi migrate` does not rewrite them.

### 4. Which languages in v1

**Recommendation:** the v0.3 list and nothing else: `fail`, `pygrep`, `python`, `rust`, `golang`, `docker_image`, plus `unsupported` / `unsupported_script` behind the policy allowlist. `docker` (build a Dockerfile from the hook repo) waits, because it executes a Dockerfile from the hook author. `conda`, `ruby`, `node`, `perl`, `r`, `lua`, `julia`, `haskell`, `swift`, `dotnet`, `coursier`, `dart` are unsupported rather than stubbed as success.

### 5. Whether Node is ever supported

**Recommendation:** not in v1, and not as a downloader in any version. If a later version runs `language: node`, the node binary must already be on `PATH`, must be named in `runtime_allow`, and its npm dependencies must be fully hashed. ahi must not grow a nodeenv. The inferred container images do not contain Node; a hook that needs Node should be replaced by a builtin or a Rust/Go/Python hook.

### 6. Relationship to prek and to upstream pre-commit

**Recommendation:** independent implementation. Do not vendor prek, do not shell out to pre-commit, and do not aim for "passes the upstream integration suite" as an acceptance test. Use upstream and prek as behavioral references for the YAML surface, and differ where those projects download code or run it unsandboxed. Contributing builtin logic upstream is fine later; coupling the roadmap to a prek release is not.

If Ian would rather extend prek with a strict mode, ahi should stop before v0.3 adds a second language runner. v0.1 is still worth having: the builtin checks and the config parser do not exist as a fail-closed tool inside prek today (prek installs toolchains and treats compatibility as the default).

### 7. SemVer versus calendar versioning

An earlier note described calendar tags (`YYYY.MM.DD`). Ian's `rfconversions` repository (tag `v0.7.7`) uses explicit SemVer, an annotated tag `vX.Y.Z`, and `scripts/cut-release.sh` that refuses to infer the next version.

**Open.** The script and the publish workflow in this repository follow that SemVer shape so there is one concrete release path. Calendar tags remain available as the other side of this decision. If Ian picks calendar tags, the script's version pattern and the tag prefix have to change together, and the cargo version has to move with them. v0.1 stays `0.1.0` and creates no tag.

## Sources

Fetched 2026-10-25 unless noted.

- Pre-commit documentation: <https://pre-commit.com/> (config schema, languages, commands, filtering, stages, cache location, `rev` immutability).
- Pre-commit.ci configuration: <https://pre-commit.ci/> and <https://github.com/pre-commit-ci/pre-commit-ci-config/blob/main/pre_commit_ci_config.py>.
- Pre-commit source: <https://github.com/pre-commit/pre-commit> at `main`, release <https://github.com/pre-commit/pre-commit/releases/tag/v4.7.0>. Files cited inline: `pre_commit/clientlib.py`, `constants.py`, `all_languages.py`, `store.py`, `xargs.py`, `lang_base.py`, `commands/run.py`, `commands/hook_impl.py`, `commands/migrate_config.py`, `staged_files_only.py`, `git.py`, `resources/hook-tmpl`, `languages/node.py`, `languages/rust.py`, `languages/golang.py`.
- identify: <https://github.com/pre-commit/identify> (`identify.py` tag API).
- pre-commit-hooks manifest and fixer sources: <https://github.com/pre-commit/pre-commit-hooks/blob/main/.pre-commit-hooks.yaml> and the `pre_commit_hooks/*.py` modules cited inline.
- prek: <https://github.com/j178/prek>, <https://prek.j178.dev/dev/compatibility/>, <https://prek.j178.dev/diff/>, <https://prek.j178.dev/dev/>.
- lefthook: <https://github.com/evilmartians/lefthook>, <https://lefthook.dev/>, <https://lefthook.dev/configuration/parallel/>, <https://lefthook.dev/configuration/remotes/>.
- hk: <https://github.com/jdx/hk>, <https://hk.jdx.dev/why-hk.html>, <https://hk.jdx.dev/migrate-lefthook.html>.
- crates.io, npm, PyPI, RubyGems, NuGet, Homebrew, AUR, Arch, and the Go module proxy, queried for the name `ahi` on 2026-10-25. See [Crate name](#2-crate-name).
- `rfconversions` tag `v0.7.7` (`510fbc30d9f7093d97a2aa2658e3ab7b6035b38a`): `.github/workflows/ci.yml`, `justfile`, `scripts/cut-release.sh`, `docs/release.md`. The `main` branch of that repository at fetch time contained only `.gitignore` and `LICENSE`. The workflow files live on the tag.
- OpenShell: [policy overview](https://docs.nvidia.com/openshell/how-it-works/policies/overview), [schema](https://docs.nvidia.com/openshell/how-it-works/policies/schema), [default policy](https://docs.nvidia.com/openshell/how-it-works/policies/default-policy), [reference schema](https://docs.nvidia.com/openshell/reference/policy-schema), [sandboxes policies](https://docs.nvidia.com/openshell/latest/sandboxes/policies), [security best practices](https://docs.nvidia.com/openshell/security/best-practices), [providers overview](https://docs.nvidia.com/openshell/how-it-works/providers/overview).
- `actions/checkout` tag `v7` resolved to commit `3d3c42e5aac5ba805825da76410c181273ba90b1` via the GitHub API on 2026-10-25. `dtolnay/rust-toolchain` commit `683b588c487a69d01f6c6ddf40f036ff682cc2c8`.

## Unverified

- Git's own changelog for "2.54 introduced `hook.<name>.event`". The sentence in this spec is pre-commit's claim.
- Byte-for-byte parity with identify's `is_text` sample window, until the ported function is tested against identify.
- The Trojan Source code-point list was not re-read from the USENIX PDF in this environment.
- Sandbox profiles were not executed. The host-backend table is from documented behavior of Landlock, bubblewrap, `sandbox-exec`, and Windows job objects, not from a measurement on Ian's machines.
- ahi was not run inside OpenShell. Nesting Landlock or seccomp was not attempted. OpenShell on macOS and on native Windows was not found in the fetched Linux docs.
- No end-to-end `git push` with an OpenShell placeholder token was executed. The provider docs describe HTTP placeholder substitution.
- No gitsign or sigstore bundle was verified here.
- Registry availability for the name `ahi` is a point-in-time response from 2026-10-25.
- Star counts for prek and hk are from the GitHub API at research time and will drift.
- Optional conformance against a live `pre-commit` binary has not been run.
- macOS and Windows CI jobs have not been executed in this environment. Linux fmt, clippy, tests, deny, audit, package, and the version smoke are the runs this environment can do.
