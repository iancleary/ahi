# Release process

`ahi` is a single Rust crate. The version source is the root `Cargo.toml` package version. The script below follows the explicit-version flow used by `iancleary/rfconversions` at tag `v0.7.7`. SemVer versus calendar versioning is an open decision in `docs/SPEC.md`. The script requires an explicit `X.Y.Z` version either way, and it tags `vX.Y.Z`. It does not infer the next version.

This repository's publish workflow runs only after a GitHub Release is published from a tag on `iancleary/ahi`. Creating a tag or a release is a deliberate later step. Development work does not run this script.

## Prerequisites

The release scripts embed short Python programs for the manifest and lockfile edits. `scripts/python_runner.sh` runs them with `uv run --python 3.14` when `uv` is installed, which is the expected local setup; `.python-version` pins the same interpreter for `uv run scripts/smoke-model-picker.py`. Without `uv`, the scripts fall back to the system `python3`, which must be 3.11 or newer for `tomllib`. CI runners take that path.

## Member crates must match their published versions

`ahi` pins `crates/ahi-win32` and `crates/ahi-widgets` by exact version, and `scripts/publish-workspace.sh` skips a member whose version crates.io already lists. That skip is only safe when the published crate is the one in the tree. A change inside a member without a version bump, which is what a Dependabot update of `crates/*/Cargo.toml` produces, passes every local check (path dependencies always agree with themselves) and then fails `cargo publish` of `ahi` at release time against the stale registry copy. That is how the 0.13.0 crate never reached crates.io.

`scripts/check-published-members.sh` closes this: for each member whose version is on the index, it downloads the published `.crate` from static.crates.io, runs `cargo package` for the member, and compares the two archives file by file (ignoring the normalized `Cargo.toml` and `.cargo_vcs_info.json` that cargo generates). Any difference fails with the files that changed and the fix: bump the member's `version`, update ahi's `=x.y.z` pin, run `cargo update -p <member> --offline`. An unpublished version passes. It runs in CI on every push (Linux), in `just check`, in `cut-release.sh` before anything is tagged, and in `publish-workspace.sh` before a member is skipped. It needs network access and no token.

## Read-only version queries

```bash
just cut-release --print-current-version
just cut-release --print-next-version
```

`--print-next-version` exits non-zero and explains that `--version` is required.

## Dry run

```bash
just cut-release --dry-run --version 0.1.1
```

The dry run updates `Cargo.toml` and `Cargo.lock`, runs validation, reports the public commands it would run, and restores the local files before exiting. It does not push, tag, or publish.

JSON and SARIF golden files record `ahi_version` and the SARIF tool version. The comparison substitutes `env!("CARGO_PKG_VERSION")` into those two fields before the byte compare, and the live command output must still report that version. Text output is compared unchanged. A version bump therefore does not by itself fail `cargo test`.

## Real release

Run a real release only from a clean `main` branch with GitHub CLI authentication available:

```bash
just cut-release --version 0.1.1
```

The runner updates the root `Cargo.toml` and `Cargo.lock`, then validates with:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`
- `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps`
- `cargo package --workspace --allow-dirty`

Then it commits the version bump, creates an annotated `vX.Y.Z` tag, pushes the branch and tag, and runs `gh release create` as the last public step. Publishing the GitHub Release starts `.github/workflows/publish.yml`, which is the only workflow that can read `CARGO_REGISTRY_TOKEN`. The same published release starts `.github/workflows/release_binary_assets.yml`, which attaches the Linux and Windows archives below. That workflow does not read `CARGO_REGISTRY_TOKEN`.

Release notes default to GitHub-generated notes. To use curated notes:

```bash
just cut-release --version 0.1.1 --notes-file path/to/notes.md
```

## Gateway retry validation

Before publishing, require successful Linux and Windows CI as well as the local release checks. The real-HTTP retry cases in [`src/chat/backend/http/tests.rs`](../src/chat/backend/http/tests.rs) can be rerun locally with:

```bash
cargo test --all-features chat::backend::http::tests::an_unreachable_gateway
cargo test --all-features chat::backend::http::tests::a_gateway_that_comes_up
cargo test --all-features chat::backend::http::tests::cancelling_during_the_backoff
```

A connection attempt includes the operating system's TCP failure detection, not just ahi's 250 ms and 750 ms backoff pauses. Windows CI took about seven seconds for three refused connections and both pauses; five seconds is not a portable upper bound for the entire turn. The unreachable-gateway case still requires both reconnect notices, a terminal transport failure, at least 900 ms elapsed, and completion within 15 seconds, well below the client's 120-second request timeout. This is a fixture bound, not a user-facing latency guarantee.

The recovery case waits for `Reconnecting 2/3` before binding its loopback listener, then checks the streamed text and successful completion. Starting the listener after a fixed sleep can let Windows' first TCP attempt succeed without ever exercising recovery. If these cases fail on a hosted runner, inspect its failed test output before publishing; a local POSIX pass does not establish Windows behavior, and rerunning a timing race is not a repair.

## Binary assets

`.github/workflows/release_binary_assets.yml` builds the tag that was published, or the tag named by `workflow_dispatch` input `release_tag`. It checks out `refs/tags/<tag>` and refuses any other ref. The root `Cargo.toml` version must equal the tag with the leading `v` removed.

The assets on that release are named:

- `ahi-<version>-linux-x86_64.tar.gz`
- `ahi-<version>-linux-aarch64.tar.gz`
- `ahi-<version>-windows-x86_64.tar.gz`
- `ahi-<version>-sha256sums.txt`

`<version>` is the SemVer string without the leading `v`. Each archive contains one executable at the archive root — `ahi` on Linux, `ahi.exe` on Windows — with no directory prefix and no other members. The Linux archives are bit-reproducible on their runner: the member is mode `0755`, numeric owner `0`, and its mtime is the tagged commit's committer timestamp, and the gzip header has no timestamp. The Windows archive is built natively on `windows-2025`, is not promised to be bit-reproducible, and uses a timestamp-free gzip header. `ahi-<version>-sha256sums.txt` is `sha256sum` output for the three archives in the order listed above, so `sha256sum --check` works in the directory that contains the files.

The Linux binaries are `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, built with Rust 1.99.0 and `cargo build --release --locked --target ...`, then stripped. `ubuntu-latest` builds `x86_64`. `ubuntu-24.04-arm` builds `aarch64`. That label is the GitHub-hosted arm64 runner in the [hosted runners reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). The build does not cross-compile. Each runner installs `musl-tools` so `ring`, pulled in by the rustls backend, can compile its C with the native `musl-gcc`. Rust's self-contained musl linker still links the final binary. `file` must report a static ELF and `ldd` must not list shared libraries. The packaged binary is executed on that same runner; `ahi --version` must print `ahi <version>` and nothing else.

`SOURCE_DATE_EPOCH` is the tagged commit's committer timestamp. `RUSTFLAGS` passes `-C target-feature=+crt-static` and `--remap-path-prefix` for the source tree, cargo home, rustc sysroot, rustup home, and the home directory, longest prefix first. Each runner builds twice from a clean target directory and fails if the stripped binaries differ. It packages the archive twice and fails if those sha256 values differ. That is same-runner reproducibility. A later binutils, a different gzip, or a different machine is not promised to reproduce the hash.

`windows-2025` builds `x86_64-pc-windows-msvc` natively with `cargo build --release --locked --target ...`. This archive is packaged once with a timestamp-free gzip header, is not promised to be bit-reproducible, and ships unsigned: Windows SmartScreen may prompt on first run of a downloaded copy. macOS archives are deferred; use `cargo install ahi` there and see [the follow-up checklist](macos-update-follow-up.md). The packaged binary is executed on that same runner; `ahi --version` must print `ahi <version>` and nothing else. Rust 1.88.0 is the declared MSRV; the release builds use the pinned 1.99.1 toolchain the workflows install.

A pull request runs the build, packaging, smoke test, reproducibility comparison, and `sha256sum --check`. It does not upload release assets and does not attest. `contents` on those jobs stays `read`. `actions: write` is set only so the runners can pass archives through workflow artifacts. `attach_release_assets` is the only job with `contents: write`, `id-token: write`, and `attestations: write`. It runs for `release: published` and for `workflow_dispatch`. It attests the four files with `actions/attest-build-provenance` and uploads an asset only when that name is absent. If the name is already present and the bytes differ, the job fails and leaves the existing asset in place.

### Upgrading from 0.3.1

The 0.3.1 updater requires exactly the two Linux archives in the checksum manifest. It will safely refuse the new three-archive manifest. Bootstrap 0.3.2 once using `cargo install ahi` or by manually downloading and verifying the appropriate archive below. From 0.3.2, `ahi update` understands the Windows/Linux manifest. Windows users likewise need a one-time install of the new executable; Cargo is not needed when installing the verified archive.

### Verify a download

```bash
mkdir -p /tmp/ahi-0.1.1 && cd /tmp/ahi-0.1.1
gh release download v0.1.1 --repo iancleary/ahi --pattern 'ahi-0.1.1-*'
sha256sum --check ahi-0.1.1-sha256sums.txt
gh attestation verify ahi-0.1.1-linux-x86_64.tar.gz --repo iancleary/ahi
gh attestation verify ahi-0.1.1-linux-aarch64.tar.gz --repo iancleary/ahi
gh attestation verify ahi-0.1.1-sha256sums.txt --repo iancleary/ahi
```

Replace `0.1.1` with the release that was downloaded.

For a release published after this workflow is on `main`, the attestation's workflow commit is the tagged commit. For a manual backfill, dispatch runs from `main`, so the attestation's workflow commit is that `main` commit while the binary is built from the tag.

### Backfill

v0.1.0 and v0.1.1 have no assets. After this workflow file is on `main`, backfill one existing release at a time. Do not create a new tag and do not run `scripts/cut-release.sh` for this.

```bash
gh workflow run release_binary_assets.yml --repo iancleary/ahi --ref main -f release_tag=v0.1.1
gh run list --repo iancleary/ahi --workflow release_binary_assets.yml --limit 5
```

Repeat with `-f release_tag=v0.1.0` for that release. Select workflow ref `main`. The old tag does not contain the workflow. The run checks out `refs/tags/<release_tag>` as the source tree.

The first release that receives assets without a manual dispatch is the next one published after the workflow file is on `main`.
