#!/usr/bin/env bash
# Refuse a member crate whose version is already on crates.io with different
# contents. ahi pins its in-repo crates (crates/ahi-win32, crates/ahi-widgets)
# by exact version, and the publish script skips a member whose version the
# registry already lists. That skip is only safe when the published crate is
# the one in the tree. A dependency bump inside a member (Dependabot edits
# crates/*/Cargo.toml without bumping the crate's version) breaks that: the
# tree builds from the path, the registry copy pins the old dependency, and
# `cargo publish` of ahi fails at release time (0.13.0, 2026-11-08).
#
# For each member: read its version; if the sparse index lists it, download
# the published .crate and compare it file by file with `cargo package`'s
# output, ignoring the two files cargo generates (the normalized Cargo.toml
# and .cargo_vcs_info.json). Any difference is an error naming the fix. An
# unpublished version passes: it will be published. No token is needed.
#
# Runs in CI on every push, in cut-release.sh before anything is tagged, and
# in publish-workspace.sh before a member is skipped.
set -euo pipefail

# shellcheck source=scripts/python_runner.sh
source "$(dirname "${BASH_SOURCE[0]}")/python_runner.sh"

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

members=(crates/ahi-win32 crates/ahi-widgets)
status=0

manifest_version() {
  sed -n 's/^version = "\(.*\)"$/\1/p' "$1" | head -n 1
}

manifest_name() {
  sed -n 's/^name = "\(.*\)"$/\1/p' "$1" | head -n 1
}

index_path() {
  local name="$1"
  case "${#name}" in
    1) echo "1/${name}" ;;
    2) echo "2/${name}" ;;
    3) echo "3/${name:0:1}/${name}" ;;
    *) echo "${name:0:2}/${name:2:2}/${name}" ;;
  esac
}

for member in "${members[@]}"; do
  manifest="$member/Cargo.toml"
  name="$(manifest_name "$manifest")"
  version="$(manifest_version "$manifest")"
  if [[ -z "$name" || -z "$version" ]]; then
    echo "check-published-members: cannot read name and version from $manifest" >&2
    exit 2
  fi
  index="https://index.crates.io/$(index_path "$name")"
  if ! curl --fail --silent --show-error "$index" 2>/dev/null | grep --quiet "\"vers\":\"${version}\""; then
    echo "${name} ${version}: not on crates.io yet; it will be published"
    continue
  fi
  work="$(mktemp -d "${TMPDIR:-/tmp}/check-published.XXXXXX")"
  trap 'rm -rf "$work"' EXIT
  published="$work/${name}-${version}.published.crate"
  curl --fail --silent --show-error --location \
    --output "$published" \
    "https://static.crates.io/crates/${name}/${name}-${version}.crate"
  # --allow-dirty: the release script runs this with the version bump
  # uncommitted; the member crates themselves are compared, not ahi.
  cargo package --quiet --allow-dirty --no-verify --package "$name" \
    --target-dir "$work/target" >/dev/null
  local_crate="$work/target/package/${name}-${version}.crate"
  if ! run_python "$name" "$version" "$published" "$local_crate" <<'PY'
import hashlib
import sys
import tarfile

name, version, published_path, local_path = sys.argv[1:5]
prefix = f"{name}-{version}/"
# cargo writes these two itself; everything else is the author's.
generated = {prefix + "Cargo.toml", prefix + ".cargo_vcs_info.json"}


def digests(path):
    out = {}
    with tarfile.open(path, "r:gz") as tar:
        for member in tar.getmembers():
            if not member.isfile() or member.name in generated:
                continue
            data = tar.extractfile(member).read()
            out[member.name] = hashlib.sha256(data).hexdigest()
    return out


published = digests(published_path)
local = digests(local_path)
if published == local:
    print(f"{name} {version}: published crate matches the tree")
    raise SystemExit(0)
print(f"{name} {version} is on crates.io but differs from the tree:", file=sys.stderr)
for file in sorted(set(published) | set(local)):
    if file not in published:
        print(f"  added    {file}", file=sys.stderr)
    elif file not in local:
        print(f"  removed  {file}", file=sys.stderr)
    elif published[file] != local[file]:
        print(f"  changed  {file}", file=sys.stderr)
print(
    f"  bump `version` in crates/{name}/Cargo.toml and ahi's `{name} = {{ ..., version = \"=...\" }}` pin,\n"
    f"  then `cargo update -p {name} --offline`; the publish script uploads the new version before ahi.",
    file=sys.stderr,
)
raise SystemExit(1)
PY
  then
    status=1
  fi
  rm -rf "$work"
  trap - EXIT
done

exit "$status"
