# Restore Cargo-free macOS updates

Deferred on 2026-11-04 for the 0.3.2 Windows/Linux patch. Revisit later this week when GitHub-hosted macOS ARM64 capacity recovers. This is a handoff, not a scheduled job.

Until then, install/update on macOS with `cargo install ahi`. `ahi update` refuses macOS before network use. Windows x86_64 and Linux x86_64/aarch64 use attested archives (the updater also requires `gh`).

## Evidence

- CI run https://github.com/iancleary/ahi/actions/runs/41860327610
- Assets run https://github.com/iancleary/ahi/actions/runs/41860327649
- Both macos-26 jobs executed zero steps and reported: "The job was not acquired by Runner of type hosted even after multiple attempts" plus macOS ARM64 capacity notices.
- macos-latest previously failed the same way. The observed annotations establish capacity trouble; they do not establish a certificate-related cause.
- Original implementation/history: PR #14, commits 11b6f6c through 4ee1b86. Windows archive packaging and CI passed.

## Restore as one verified change

1. Restore a working hosted macOS ARM64 label to both `.github/workflows/ci.yml` and `.github/workflows/publish.yml`, and to the native release matrix (`aarch64-apple-darwin`, architecture `darwin-aarch64`, binary `ahi`). Do not label an Intel build as ARM64.
2. Restore the Darwin archive in `scripts/write_sha256sums.sh`, `scripts/attach_release_assets.sh`, and the release workflow's attestation subjects.
3. Restore the matching `TARGET_SUFFIXES` entry and macOS ARM64 mapping in `src/update.rs`. Keep the producer and strict checksum parser's complete archive sets identical.
4. Update the supported-host CLI test, artifact mapping test, README, release docs, SPEC, and changelog together.
5. Require real macOS CI and native archive build/package/version smoke success, plus the complete checksum aggregation job. Verify an actual attested release update on macOS without Cargo before claiming end-to-end support. Retain verification failures as failures.

The native macOS archive design is unsigned and does not promise bit reproducibility. Reconsider signing separately rather than blocking the capacity recovery on it.
