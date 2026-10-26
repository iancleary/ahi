#!/usr/bin/env bash
# Build a stripped, statically linked release binary twice and compare sha256.
#
# The working directory is the crate root. Both builds use one target-directory
# path, a clean directory, SOURCE_DATE_EPOCH, and the same rustc flags.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/build_linux_binary.sh --target <triple> --output <path> --version <semver> --source-date-epoch <unix> --expect-commit <sha>

The target must be x86_64-unknown-linux-musl or aarch64-unknown-linux-musl.
The script runs: cargo build --release --locked --target <triple>
USAGE
}

target=""
output=""
version=""
source_date_epoch=""
expect_commit=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      target=${2:-}
      shift 2
      ;;
    --output)
      output=${2:-}
      shift 2
      ;;
    --version)
      version=${2:-}
      shift 2
      ;;
    --source-date-epoch)
      source_date_epoch=${2:-}
      shift 2
      ;;
    --expect-commit)
      expect_commit=${2:-}
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

if [[ -z "$target" || -z "$output" || -z "$version" || -z "$source_date_epoch" || -z "$expect_commit" ]]; then
  usage >&2
  exit 2
fi

case "$target" in
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) ;;
  *)
    echo "error: target must be a linux musl triple: ${target}" >&2
    exit 2
    ;;
esac

if [[ ! "$source_date_epoch" =~ ^[0-9]+$ ]]; then
  echo "error: source-date-epoch must be a unix timestamp" >&2
  exit 2
fi

if [[ ! -f Cargo.toml ]]; then
  echo "error: Cargo.toml is not in the working directory" >&2
  exit 1
fi

source_root=$(pwd -P)
cd "$source_root"

head=$(git rev-parse HEAD)
if [[ "$head" != "$expect_commit" ]]; then
  echo "error: HEAD ${head} does not match expected commit ${expect_commit}" >&2
  exit 1
fi

commit_epoch=$(git log -1 --format=%ct)
if [[ "$commit_epoch" != "$source_date_epoch" ]]; then
  echo "error: commit timestamp ${commit_epoch} does not match source-date-epoch ${source_date_epoch}" >&2
  exit 1
fi

manifest_version=$(
  python3 - <<'PY'
import pathlib
import tomllib

manifest = pathlib.Path("Cargo.toml")
print(tomllib.loads(manifest.read_text())["package"]["version"])
PY
)
if [[ "$manifest_version" != "$version" ]]; then
  echo "error: Cargo.toml version ${manifest_version} does not match ${version}" >&2
  exit 1
fi

reject_remap_path() {
  local path=$1
  if [[ "$path" == *' '* || "$path" == *'='* || "$path" == *$'\n'* ]]; then
    echo "error: cannot remap path: ${path}" >&2
    exit 1
  fi
}

declare -A remap_labels=()
add_remap() {
  local path=$1
  local label=$2
  if [[ -z "$path" || ! -d "$path" ]]; then
    return 0
  fi
  path=$(cd "$path" && pwd -P)
  reject_remap_path "$path"
  if [[ -n "${remap_labels[$path]:-}" ]]; then
    return 0
  fi
  remap_labels["$path"]=$label
}

add_remap "$source_root" /source
add_remap "${CARGO_HOME:-${HOME}/.cargo}" /cargo
add_remap "$(rustc --print sysroot)" /rustc-sysroot
if [[ -n "${RUSTUP_HOME:-}" ]]; then
  add_remap "$RUSTUP_HOME" /rustup
fi
add_remap "$HOME" /home

if [[ ${#remap_labels[@]} -eq 0 ]]; then
  echo "error: no paths to remap" >&2
  exit 1
fi

remap_entries=()
for path in "${!remap_labels[@]}"; do
  printf -v entry '%08d|%s|%s' "${#path}" "$path" "${remap_labels[$path]}"
  remap_entries+=("$entry")
done

# Longer prefixes first. rustc applies the first matching --remap-path-prefix.
mapfile -t remap_entries < <(printf '%s\n' "${remap_entries[@]}" | sort -r)

rustflags=()
for entry in "${remap_entries[@]}"; do
  rest=${entry#*|}
  path=${rest%%|*}
  label=${rest#*|}
  if [[ -z "$path" || -z "$label" || "$path" == "$label" ]]; then
    echo "error: bad remap entry: ${entry}" >&2
    exit 1
  fi
  rustflags+=("--remap-path-prefix=${path}=${label}")
  echo "build_linux_binary: remap ${path} -> ${label}"
done
rustflags+=("-C" "target-feature=+crt-static")

if [[ -n "${RUSTFLAGS:-}" ]]; then
  echo "build_linux_binary: replacing inherited RUSTFLAGS" >&2
fi
export RUSTFLAGS="${rustflags[*]}"
unset CARGO_ENCODED_RUSTFLAGS || true
export SOURCE_DATE_EPOCH="$source_date_epoch"
export CARGO_INCREMENTAL=0

target_dir="${source_root}/target/ahi-release-musl"
tmp_dir="${source_root}/target/ahi-release-tmp"
export CARGO_TARGET_DIR="$target_dir"

build_once() {
  local dest=$1
  rm -rf "$target_dir" "$tmp_dir"
  mkdir -p "$tmp_dir"
  # Cargo and build scripts honor TMPDIR. The directory exists before the build,
  # and the path is inside the source tree so the remap covers it.
  export TMPDIR="$tmp_dir"
  cargo build --release --locked --target "$target"
  local built="${target_dir}/${target}/release/ahi"
  if [[ ! -f "$built" ]]; then
    echo "error: binary was not produced at ${built}" >&2
    exit 1
  fi
  strip --strip-all "$built"
  mkdir -p "$(dirname "$dest")"
  cp "$built" "$dest"
  chmod 0755 "$dest"
}

if ! work=$(mktemp -d -p /tmp ahi-release-binary.XXXXXX); then
  echo "error: mktemp failed" >&2
  exit 1
fi
trap 'rm -rf "$work"' EXIT
build_once "$work/first"
build_once "$work/second"

hash1=$(sha256sum "$work/first" | awk '{print $1}')
hash2=$(sha256sum "$work/second" | awk '{print $1}')
echo "build_linux_binary: first sha256 ${hash1}"
echo "build_linux_binary: second sha256 ${hash2}"
if [[ "$hash1" != "$hash2" ]]; then
  echo "error: stripped binary sha256 values differ" >&2
  exit 1
fi
echo "build_linux_binary: stripped binary sha256 values match"

mkdir -p "$(dirname "$output")"
cp "$work/first" "$output"
chmod 0755 "$output"
rm -rf "$target_dir" "$tmp_dir"
