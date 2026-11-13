# list recipes
help:
    just --list

# format the code
fmt:
    cargo fmt --all

# alias for fmt
format: fmt

# check formatting without writing changes
fmt-check:
    cargo fmt --all -- --check

# lint the code without writing changes
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# apply automatic clippy fixes
lint-fix:
    cargo clippy --all-targets --all-features --fix -- -D warnings

# run tests
test:
    cargo test --all-features

# check documentation with rustdoc warnings denied
doc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps

# verify package contents without publishing (the member crates are packaged with ahi)
package:
    cargo package --workspace

# fail when a member crate's published version differs from the tree (needs network)
check-published:
    bash scripts/check-published-members.sh

# build the crate
build:
    cargo build --release

# format, lint, test, document, and package like CI
check: fmt-check lint test doc-check package check-published

# run the same checks and build mirrored by CI, plus supply-chain checks and the version smoke
ci: check build
    bash scripts/install-supply-chain-tools.sh
    cargo deny check
    cargo audit
    bash scripts/smoke-version.sh

# Cut a GitHub release for an explicit SemVer version.
# Do not run this recipe as part of ordinary development. It tags, pushes, and opens a GitHub release.
cut-release *args:
    ./scripts/cut-release.sh {{args}}

# run the crate
dev:
    cargo run
