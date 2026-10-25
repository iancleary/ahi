# Release process

`ahi` is a single Rust crate. The version source is the root `Cargo.toml` package version. The script below follows the explicit-version flow used by `iancleary/rfconversions` at tag `v0.7.7`. SemVer versus calendar versioning is an open decision in `docs/SPEC.md`. The script requires an explicit `X.Y.Z` version either way, and it tags `vX.Y.Z`. It does not infer the next version.

This repository's publish workflow runs only after a GitHub Release is published from a tag on `iancleary/ahi`. Creating a tag or a release is a deliberate later step. Development work does not run this script.

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
- `cargo package --allow-dirty`

Then it commits the version bump, creates an annotated `vX.Y.Z` tag, pushes the branch and tag, and runs `gh release create` as the last public step. Publishing the GitHub Release starts `.github/workflows/publish.yml`, which is the only workflow that can read `CARGO_REGISTRY_TOKEN`.

Release notes default to GitHub-generated notes. To use curated notes:

```bash
just cut-release --version 0.1.1 --notes-file path/to/notes.md
```
