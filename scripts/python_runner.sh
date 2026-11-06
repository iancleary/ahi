#!/usr/bin/env bash
# Sourced by scripts that embed Python. Runs the interpreter through uv with
# the repository's pinned Python (see .python-version) when uv is installed,
# and otherwise falls back to the system python3 so CI runners without uv keep
# working. Both paths read the program from stdin and forward "$@" to it.

# shellcheck disable=SC2120
run_python() {
  if command -v uv >/dev/null 2>&1; then
    uv run --no-project --python 3.14 python - "$@"
  else
    python3 - "$@"
  fi
}
