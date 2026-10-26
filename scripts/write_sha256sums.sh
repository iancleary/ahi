#!/usr/bin/env bash
# Write ahi-<version>-sha256sums.txt in sha256sum format and check it.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/write_sha256sums.sh --version <semver> --directory <path>

The directory must contain:
  ahi-<version>-linux-x86_64.tar.gz
  ahi-<version>-linux-aarch64.tar.gz

The sums file lists the x86_64 archive first, then the aarch64 archive.
USAGE
}

version=""
directory=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version=${2:-}
      shift 2
      ;;
    --directory)
      directory=${2:-}
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

if [[ -z "$version" || -z "$directory" ]]; then
  usage >&2
  exit 2
fi

x86_name="ahi-${version}-linux-x86_64.tar.gz"
arm_name="ahi-${version}-linux-aarch64.tar.gz"
sums_name="ahi-${version}-sha256sums.txt"

if [[ ! -f "$directory/$x86_name" ]]; then
  echo "error: missing ${x86_name}" >&2
  exit 1
fi
if [[ ! -f "$directory/$arm_name" ]]; then
  echo "error: missing ${arm_name}" >&2
  exit 1
fi

(
  cd "$directory"
  sha256sum "$x86_name" "$arm_name" >"$sums_name"
  sha256sum --check "$sums_name"
)

python3 - "$directory/$sums_name" "$x86_name" "$arm_name" <<'PY'
import pathlib
import re
import sys

path, x86_name, arm_name = sys.argv[1:]
text = pathlib.Path(path).read_text()
lines = text.splitlines()
if text != "\n".join(lines) + "\n":
    raise SystemExit("sha256sums file must end with one newline and contain no extra blank line")
if len(lines) != 2:
    raise SystemExit(f"sha256sums file has {len(lines)} lines, expected 2")
pattern = re.compile(r"^[0-9a-f]{64}  [^ ].+$")
names = []
for line in lines:
    if not pattern.fullmatch(line):
        raise SystemExit(f"sha256sums line is not sha256sum text format: {line!r}")
    names.append(line.split("  ", 1)[1])
if names != [x86_name, arm_name]:
    raise SystemExit(f"sha256sums filenames are {names}, expected {[x86_name, arm_name]}")
PY

echo "write_sha256sums: ${directory}/${sums_name}"
