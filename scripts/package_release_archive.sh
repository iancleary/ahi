#!/usr/bin/env bash
# Write ahi-<version>-linux-<architecture>.tar.gz with one root member, ahi.
#
# Metadata is fixed: ustar, numeric owner 0, mode 0755, mtime from
# --source-date-epoch, members sorted by name, gzip without a timestamp.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package_release_archive.sh --binary <path> --version <semver> --architecture <x86_64|aarch64> --source-date-epoch <unix> --output <path>
USAGE
}

binary=""
version=""
architecture=""
source_date_epoch=""
output=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      binary=${2:-}
      shift 2
      ;;
    --version)
      version=${2:-}
      shift 2
      ;;
    --architecture)
      architecture=${2:-}
      shift 2
      ;;
    --source-date-epoch)
      source_date_epoch=${2:-}
      shift 2
      ;;
    --output)
      output=${2:-}
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

if [[ -z "$binary" || -z "$version" || -z "$architecture" || -z "$source_date_epoch" || -z "$output" ]]; then
  usage >&2
  exit 2
fi

case "$architecture" in
  x86_64 | aarch64) ;;
  *)
    echo "error: architecture must be x86_64 or aarch64" >&2
    exit 2
    ;;
esac

if [[ ! "$source_date_epoch" =~ ^[0-9]+$ ]]; then
  echo "error: source-date-epoch must be a unix timestamp" >&2
  exit 2
fi

if [[ ! -f "$binary" ]]; then
  echo "error: binary not found: ${binary}" >&2
  exit 1
fi

expected_name="ahi-${version}-linux-${architecture}.tar.gz"
if [[ "$(basename "$output")" != "$expected_name" ]]; then
  echo "error: output basename must be ${expected_name}" >&2
  exit 1
fi

if ! tar --version | head -n 1 | grep -q 'GNU tar'; then
  echo "error: GNU tar is required" >&2
  exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$binary" "$stage/ahi"
chmod 0755 "$stage/ahi"
touch -m -d "@${source_date_epoch}" "$stage/ahi"

mkdir -p "$(dirname "$output")"
tar --format=ustar \
  --sort=name \
  --mtime="@${source_date_epoch}" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --mode=0755 \
  -C "$stage" \
  -cf - ahi | gzip -n -9 >"$output"

python3 - "$output" "$source_date_epoch" <<'PY'
import sys
import tarfile

path, epoch = sys.argv[1], int(sys.argv[2])
with open(path, "rb") as handle:
    header = handle.read(10)
if header[:2] != b"\x1f\x8b":
    raise SystemExit("gzip magic is missing")
mtime = int.from_bytes(header[4:8], "little")
if mtime != 0:
    raise SystemExit(f"gzip timestamp is {mtime}, expected 0")
# Flag bit 3 is FNAME and bit 4 is FCOMMENT. gzip -n clears both.
if header[3] & 0x08 or header[3] & 0x10:
    raise SystemExit(f"gzip header flags {header[3]:#x} store a name or comment")
with tarfile.open(path, "r:gz") as archive:
    members = archive.getmembers()
if len(members) != 1:
    raise SystemExit(f"archive has {len(members)} members, expected 1")
member = members[0]
if member.name != "ahi":
    raise SystemExit(f"archive member name is {member.name!r}, expected 'ahi'")
if not member.isfile():
    raise SystemExit("archive member is not a regular file")
if member.uid != 0 or member.gid != 0:
    raise SystemExit(f"archive owner is {member.uid}:{member.gid}, expected 0:0")
if member.uname or member.gname:
    raise SystemExit(f"archive owner names are {member.uname!r}:{member.gname!r}")
if member.mtime != epoch:
    raise SystemExit(f"archive mtime is {member.mtime}, expected {epoch}")
if member.mode & 0o777 != 0o755:
    raise SystemExit(f"archive mode is {member.mode & 0o777:o}, expected 755")
PY

echo "package_release_archive: ${output}"
