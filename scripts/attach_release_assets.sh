#!/usr/bin/env bash
# Attach release archives only when an existing asset is absent or identical.
#
# --mode check compares bytes and uploads nothing.
# --mode upload repeats the comparison, then uploads assets that are absent.
# An existing asset with different bytes is an error in both modes.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/attach_release_assets.sh --mode check|upload --tag <vX.Y.Z> --directory <path> --repository <owner/name>

The directory must contain the three release assets for the tag's SemVer.
GH_TOKEN or GITHUB_TOKEN must be set. The script does not pass --clobber.
USAGE
}

mode=""
tag=""
directory=""
repository=${GITHUB_REPOSITORY:-}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      mode=${2:-}
      shift 2
      ;;
    --tag)
      tag=${2:-}
      shift 2
      ;;
    --directory)
      directory=${2:-}
      shift 2
      ;;
    --repository)
      repository=${2:-}
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

if [[ "$mode" != "check" && "$mode" != "upload" ]]; then
  usage >&2
  exit 2
fi
if [[ -z "$tag" || -z "$directory" || -z "$repository" ]]; then
  usage >&2
  exit 2
fi
if [[ "${EVENT_NAME:-}" == "pull_request" ]]; then
  echo "error: refused: a pull request cannot attach release assets" >&2
  exit 1
fi
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: refused: tag is not SemVer: ${tag}" >&2
  exit 1
fi

export GH_TOKEN=${GH_TOKEN:-${GITHUB_TOKEN:-}}
if [[ -z "$GH_TOKEN" ]]; then
  echo "error: GH_TOKEN is required" >&2
  exit 1
fi

version=${tag#v}
names=(
  "ahi-${version}-linux-x86_64.tar.gz"
  "ahi-${version}-linux-aarch64.tar.gz"
  "ahi-${version}-sha256sums.txt"
)

for name in "${names[@]}"; do
  if [[ ! -f "$directory/$name" ]]; then
    echo "error: built asset is missing: ${name}" >&2
    exit 1
  fi
done

if ! asset_names=$(gh release view "$tag" --repo "$repository" --json assets --jq '.assets[].name'); then
  echo "error: GitHub release ${tag} was not found in ${repository}" >&2
  exit 1
fi

asset_present() {
  local name=$1
  if [[ -z "$asset_names" ]]; then
    return 1
  fi
  grep -Fxq -- "$name" <<<"$asset_names"
}

download_dir=$(mktemp -d)
trap 'rm -rf "$download_dir"' EXIT

missing=()
for name in "${names[@]}"; do
  if asset_present "$name"; then
    rm -rf "$download_dir"
    mkdir -p "$download_dir"
    gh release download "$tag" --repo "$repository" --dir "$download_dir" --pattern "$name"
    if [[ ! -f "$download_dir/$name" ]]; then
      echo "error: download of existing asset ${name} did not produce that file" >&2
      exit 1
    fi
    if ! cmp -s "$directory/$name" "$download_dir/$name"; then
      echo "error: asset ${name} already exists and differs" >&2
      sha256sum "$directory/$name" "$download_dir/$name" >&2
      exit 1
    fi
    echo "attach_release_assets: ${name} already present with identical bytes"
  else
    echo "attach_release_assets: ${name} is absent"
    missing+=("$directory/$name")
  fi
done

if [[ ${#missing[@]} -eq 0 ]]; then
  echo "attach_release_assets: all assets already present with identical bytes"
  exit 0
fi

if [[ "$mode" == "check" ]]; then
  echo "attach_release_assets: check passed; ${#missing[@]} asset(s) would be uploaded"
  exit 0
fi

gh release upload "$tag" --repo "$repository" "${missing[@]}"
echo "attach_release_assets: uploaded ${#missing[@]} asset(s)"
