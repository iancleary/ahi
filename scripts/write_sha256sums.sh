#!/usr/bin/env bash
# Write ahi-<version>-sha256sums.txt in sha256sum format and check it.
set -euo pipefail

# shellcheck source=scripts/python_runner.sh
source "$(dirname "${BASH_SOURCE[0]}")/python_runner.sh"

usage() {
  cat <<'USAGE'
Usage: scripts/write_sha256sums.sh --version <semver> --directory <path>

The directory must contain the three published archives:
  ahi-<version>-linux-x86_64.tar.gz
  ahi-<version>-linux-aarch64.tar.gz
  ahi-<version>-windows-x86_64.tar.gz

The sums file lists them in that order.
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

names=(
  "ahi-${version}-linux-x86_64.tar.gz"
  "ahi-${version}-linux-aarch64.tar.gz"
  "ahi-${version}-windows-x86_64.tar.gz"
)
sums_name="ahi-${version}-sha256sums.txt"

for name in "${names[@]}"; do
  if [[ ! -f "$directory/$name" ]]; then
    echo "error: missing ${name}" >&2
    exit 1
  fi
done

(
  cd "$directory"
  sha256sum "${names[@]}" >"$sums_name"
  sha256sum --check "$sums_name"
)

run_python "$directory/$sums_name" "${names[@]}" <<'PY'
import pathlib
import re
import sys

path, *names = sys.argv[1:]
text = pathlib.Path(path).read_text()
lines = text.splitlines()
if text != "\n".join(lines) + "\n":
    raise SystemExit("sha256sums file must end with one newline and contain no extra blank line")
if len(lines) != len(names):
    raise SystemExit(f"sha256sums file has {len(lines)} lines, expected {len(names)}")
pattern = re.compile(r"^[0-9a-f]{64}  [^ ].+$")
listed = []
for line in lines:
    if not pattern.fullmatch(line):
        raise SystemExit(f"sha256sums line is not sha256sum text format: {line!r}")
    listed.append(line.split("  ", 1)[1])
if listed != names:
    raise SystemExit(f"sha256sums filenames are {listed}, expected {names}")
PY

echo "write_sha256sums: ${directory}/${sums_name}"
