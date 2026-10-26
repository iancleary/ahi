#!/usr/bin/env bash
# Extract a release archive and require `ahi --version` to be `ahi <version>`.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify_packaged_binary.sh --archive <path> --version <semver>
USAGE
}

archive=""
version=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive)
      archive=${2:-}
      shift 2
      ;;
    --version)
      version=${2:-}
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$archive" || -z "$version" || ! -f "$archive" ]]; then
  usage >&2
  exit 2
fi

mapfile -t entries < <(tar -tzf "$archive" | sort)
if [[ ${#entries[@]} -ne 1 || "${entries[0]}" != "ahi" ]]; then
  echo "error: archive must contain exactly one member named ahi" >&2
  printf 'member: %s\n' "${entries[@]}" >&2
  exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
tar -xzf "$archive" -C "$stage"
if [[ ! -f "$stage/ahi" ]]; then
  echo "error: extracted ahi is missing" >&2
  exit 1
fi
if [[ ! -x "$stage/ahi" ]]; then
  echo "error: extracted ahi is not executable" >&2
  exit 1
fi

set +e
captured=$(
  "$stage/ahi" --version
  status=$?
  printf x
  exit "$status"
)
status=$?
set -e
if [[ $status -ne 0 ]]; then
  echo "error: ahi --version exited ${status}" >&2
  exit 1
fi
captured=${captured%x}
printf -v expected 'ahi %s\n' "$version"
if [[ "$captured" != "$expected" ]]; then
  echo "error: packaged binary version does not match ${version}" >&2
  printf 'expected: %q\n' "$expected" >&2
  printf 'actual:   %q\n' "$captured" >&2
  exit 1
fi

echo "verify_packaged_binary: ahi ${version}"
