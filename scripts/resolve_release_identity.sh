#!/usr/bin/env bash
# Select and verify the git ref a release-asset build compiles.
#
# --select-ref prints the ref to check out. It does not need a source tree.
# --verify reads the source tree in the working directory after that checkout.
# Publish modes require a tag. A pull request is a dry run and is not a tag.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/resolve_release_identity.sh --select-ref
       scripts/resolve_release_identity.sh --verify

--select-ref reads EVENT_NAME, GITHUB_REF, GITHUB_SHA, and RELEASE_TAG.
--verify reads those variables plus SOURCE_REF, then reads Cargo.toml and git
metadata in the working directory.

Outputs mode, tag, version, source_ref, source_sha, and source_date_epoch.
In GitHub Actions the same keys are appended to GITHUB_OUTPUT.
USAGE
}

emit() {
  local key=$1
  local value=$2
  if [[ "$value" == *$'\n'* ]]; then
    echo "error: output ${key} contains a newline" >&2
    exit 1
  fi
  printf 'resolve_release_identity: %s=%s\n' "$key" "$value"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s=%s\n' "$key" "$value" >>"$GITHUB_OUTPUT"
  fi
}

semver_tag() {
  local tag=$1
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]
}

manifest_version() {
  python3 - <<'PY'
import pathlib
import tomllib

manifest = pathlib.Path("Cargo.toml")
print(tomllib.loads(manifest.read_text())["package"]["version"])
PY
}

select_ref() {
  local event=${EVENT_NAME:?EVENT_NAME is required}
  case "$event" in
    pull_request)
      if [[ -z "${GITHUB_SHA:-}" ]]; then
        echo "error: GITHUB_SHA is required for a pull request" >&2
        exit 1
      fi
      case "${GITHUB_REF:-}" in
        refs/tags/*)
          echo "error: refused: pull request ref is a tag: ${GITHUB_REF}" >&2
          exit 1
          ;;
      esac
      emit source_ref "$GITHUB_SHA"
      ;;
    release)
      case "${GITHUB_REF:-}" in
        refs/tags/v*) ;;
        *)
          echo "error: refused: release ref is not a tag: ${GITHUB_REF:-}" >&2
          exit 1
          ;;
      esac
      if ! semver_tag "${GITHUB_REF#refs/tags/}"; then
        echo "error: refused: release ref is not a SemVer tag: ${GITHUB_REF}" >&2
        exit 1
      fi
      emit source_ref "$GITHUB_REF"
      ;;
    workflow_dispatch)
      if [[ -z "${RELEASE_TAG:-}" ]]; then
        echo "error: release_tag is required" >&2
        exit 1
      fi
      if ! semver_tag "$RELEASE_TAG"; then
        echo "error: refused: release_tag is not a SemVer tag: ${RELEASE_TAG}" >&2
        exit 1
      fi
      emit source_ref "refs/tags/${RELEASE_TAG}"
      ;;
    *)
      echo "error: refused: unsupported event: ${event}" >&2
      exit 1
      ;;
  esac
}

remote_tag_commit() {
  local tag=$1
  local ls_out
  # An exact tag query does not include the peeled commit. Ask for both
  # the tag and its peeled form. A lightweight tag has only the first line.
  if ! ls_out=$(git ls-remote origin "refs/tags/${tag}" "refs/tags/${tag}^{}"); then
    echo "error: git ls-remote failed for refs/tags/${tag}" >&2
    exit 1
  fi
  if [[ -z "$ls_out" ]]; then
    echo "error: refused: origin has no tag ${tag}" >&2
    exit 1
  fi
  local sha ref lightweight="" peeled=""
  while IFS=$'\t' read -r sha ref; do
    [[ -z "$sha" ]] && continue
    if [[ "$ref" == "refs/tags/${tag}^{}" ]]; then
      peeled=$sha
    elif [[ "$ref" == "refs/tags/${tag}" ]]; then
      lightweight=$sha
    fi
  done <<<"$ls_out"
  if [[ -n "$peeled" ]]; then
    printf '%s\n' "$peeled"
  elif [[ -n "$lightweight" ]]; then
    printf '%s\n' "$lightweight"
  else
    echo "error: refused: origin tag ${tag} did not resolve to a commit" >&2
    exit 1
  fi
}

verify_identity() {
  local event=${EVENT_NAME:?EVENT_NAME is required}
  local source_ref=${SOURCE_REF:?SOURCE_REF is required}
  local head epoch version tag semver remote_commit

  if [[ ! -f Cargo.toml ]]; then
    echo "error: Cargo.toml is not in the working directory" >&2
    exit 1
  fi
  head=$(git rev-parse HEAD)
  epoch=$(git log -1 --format=%ct)
  version=$(manifest_version)
  if [[ -z "$version" || -z "$epoch" || -z "$head" ]]; then
    echo "error: failed to read version, commit, or commit timestamp" >&2
    exit 1
  fi

  case "$event" in
    pull_request)
      if [[ "$source_ref" == refs/tags/* ]]; then
        echo "error: refused: pull request source ref is a tag: ${source_ref}" >&2
        exit 1
      fi
      if [[ "$head" != "$source_ref" ]]; then
        echo "error: HEAD ${head} does not match source ref ${source_ref}" >&2
        exit 1
      fi
      emit mode dry_run
      emit tag ""
      emit version "$version"
      emit source_ref "$head"
      emit source_sha "$head"
      emit source_date_epoch "$epoch"
      ;;
    release | workflow_dispatch)
      if [[ "$event" == "release" ]]; then
        case "${GITHUB_REF:-}" in
          refs/tags/*) ;;
          *)
            echo "error: refused: release ref is not a tag: ${GITHUB_REF:-}" >&2
            exit 1
            ;;
        esac
        if [[ "$source_ref" != "$GITHUB_REF" ]]; then
          echo "error: source ref ${source_ref} does not match release ref ${GITHUB_REF}" >&2
          exit 1
        fi
        tag=${GITHUB_REF#refs/tags/}
      else
        if [[ -z "${RELEASE_TAG:-}" ]]; then
          echo "error: release_tag is required" >&2
          exit 1
        fi
        tag=$RELEASE_TAG
        if [[ "$source_ref" != "refs/tags/${tag}" ]]; then
          echo "error: source ref ${source_ref} does not match refs/tags/${tag}" >&2
          exit 1
        fi
      fi
      if ! semver_tag "$tag"; then
        echo "error: refused: tag is not SemVer: ${tag}" >&2
        exit 1
      fi
      semver=${tag#v}
      if [[ "$version" != "$semver" ]]; then
        echo "error: Cargo.toml version ${version} does not match tag ${tag}" >&2
        exit 1
      fi
      remote_commit=$(remote_tag_commit "$tag")
      if [[ "$remote_commit" != "$head" ]]; then
        echo "error: refused: HEAD ${head} is not tag ${tag} (${remote_commit})" >&2
        exit 1
      fi
      emit mode publish
      emit tag "$tag"
      emit version "$version"
      emit source_ref "$source_ref"
      emit source_sha "$head"
      emit source_date_epoch "$epoch"
      ;;
    *)
      echo "error: refused: unsupported event: ${event}" >&2
      exit 1
      ;;
  esac
}

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 2
fi

case "$1" in
  --select-ref) select_ref ;;
  --verify) verify_identity ;;
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
