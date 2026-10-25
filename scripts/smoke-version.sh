#!/usr/bin/env bash
# Require `ahi --version` to equal `ahi ` plus the version in the root manifest.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

if [[ -z "$ver" ]]; then
  echo "smoke: could not read version from Cargo.toml" >&2
  exit 1
fi

bin=""
for candidate in "$root/target/release/ahi" "$root/target/debug/ahi"; do
  if [[ -x "$candidate" || -f "$candidate" ]]; then
    bin="$candidate"
    break
  fi
  if [[ -f "${candidate}.exe" ]]; then
    bin="${candidate}.exe"
    break
  fi
done
if [[ -z "$bin" ]]; then
  echo "smoke: ahi binary is missing; build it first" >&2
  exit 1
fi

actual=$("$bin" --version)
printf -v expected 'ahi %s' "$ver"
if [[ "$actual" != "$expected" ]]; then
  echo "smoke: expected exact version line:" >&2
  printf '%s\n' "$expected" >&2
  echo "smoke: actual:" >&2
  printf '%s\n' "$actual" >&2
  exit 1
fi
echo "smoke: ahi ${ver}"
