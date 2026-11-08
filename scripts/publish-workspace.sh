#!/usr/bin/env bash
# Publish the workspace to crates.io in dependency order.
#
# ahi depends on the in-repo ahi-win32 and ahi-widgets crates, so each must
# exist on the registry at its pinned version before ahi can be published.
# They rarely change, so their versions are usually already published; that
# is detected via the sparse index and skipped, because a re-publish of an
# existing version is an error. cargo publish waits for the index to list a crate before
# returning, so the second publish sees the first.
#
# Needs CARGO_REGISTRY_TOKEN in the environment. Pass --dry-run to verify
# both crates without uploading.
set -euo pipefail

dry_run=""
if [[ "${1:-}" == "--dry-run" ]]; then
  dry_run="--dry-run"
fi

manifest_version() {
  # The first `version = "..."` line of a manifest, with the quotes removed.
  sed -n 's/^version = "\(.*\)"$/\1/p' "$1" | head -n 1
}

published() {
  # Sparse-index layout: /1/<name>, /2/<name>, /3/<first char>/<name>, and
  # /<first two>/<next two>/<name> for longer names. 404 means never published.
  local name="$1" version="$2" prefix
  case "${#name}" in
    1) prefix="1" ;;
    2) prefix="2" ;;
    3) prefix="3/${name:0:1}" ;;
    *) prefix="${name:0:2}/${name:2:2}" ;;
  esac
  local index="https://index.crates.io/${prefix}/${name}"
  curl --fail --silent --show-error "$index" 2>/dev/null \
    | grep --quiet "\"vers\":\"${version}\""
}

publish_member() {
  # Publish an in-repo crate ahi depends on, unless its pinned version is
  # already on the registry (a re-publish of an existing version is an error).
  local name="$1" manifest="$2" version
  version="$(manifest_version "$manifest")"
  if published "$name" "$version"; then
    echo "${name} ${version} is already on crates.io; skipping"
  elif [[ -n "$dry_run" ]]; then
    cargo publish --verbose --dry-run --package "$name"
  else
    cargo publish --verbose --package "$name"
  fi
}

publish_member ahi-win32 crates/ahi-win32/Cargo.toml
publish_member ahi-widgets crates/ahi-widgets/Cargo.toml

# shellcheck disable=SC2086
cargo publish --verbose $dry_run --package ahi
