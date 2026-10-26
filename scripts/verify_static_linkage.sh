#!/usr/bin/env bash
# Require file(1) and ldd to describe a stripped, statically linked ELF.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify_static_linkage.sh --binary <path>
USAGE
}

binary=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      binary=${2:-}
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

if [[ -z "$binary" || ! -f "$binary" ]]; then
  usage >&2
  exit 2
fi

file_out=$(file -b "$binary")
echo "verify_static_linkage: file: ${file_out}"
case "$file_out" in
  *ELF*"statically linked"* | *ELF*"static-pie linked"*) ;;
  *)
    echo "error: binary is not a statically linked ELF" >&2
    exit 1
    ;;
esac
if [[ "$file_out" == *"not stripped"* || "$file_out" != *"stripped"* ]]; then
  echo "error: binary is not stripped" >&2
  exit 1
fi

set +e
ldd_out=$(ldd "$binary" 2>&1)
ldd_status=$?
set -e
echo "verify_static_linkage: ldd exit ${ldd_status}: ${ldd_out}"
if grep -q '=>' <<<"$ldd_out"; then
  echo "error: ldd reported shared libraries" >&2
  exit 1
fi
case "$ldd_out" in
  *"not a dynamic executable"* | *"statically linked"*) ;;
  *)
    echo "error: ldd did not report a static executable" >&2
    exit 1
    ;;
esac
