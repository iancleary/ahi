#!/usr/bin/env bash
# Install pinned cargo-deny and cargo-audit release binaries.
# cargo-deny 0.17, which builds on the 1.83 MSRV, cannot parse CVSS 4.0
# records in the current RustSec database. These upstream builds can.
# Checksums were recorded from the published .sha256 sidecars (cargo-deny)
# and from sha256sum of the release archives (cargo-audit) on 2026-10-25.
set -euo pipefail

dest="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$dest"

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux) platform_os=linux ;;
  Darwin) platform_os=macos ;;
  MINGW*|MSYS*|CYGWIN*) platform_os=windows ;;
  *) echo "install-supply-chain-tools: unsupported OS $os" >&2; exit 1 ;;
esac
case "$arch" in
  x86_64|amd64) platform_arch=x86_64 ;;
  aarch64|arm64) platform_arch=aarch64 ;;
  *) echo "install-supply-chain-tools: unsupported arch $arch" >&2; exit 1 ;;
esac

# name url sha256
tools=()
case "${platform_os}-${platform_arch}" in
  linux-x86_64)
    tools+=(
      "cargo-deny|https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz|9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"
      "cargo-audit|https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-gnu-v0.22.2.tgz|ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39"
    )
    ;;
  linux-aarch64)
    echo "install-supply-chain-tools: no pinned cargo-audit build for aarch64 linux" >&2
    exit 1
    ;;
  macos-x86_64)
    tools+=(
      "cargo-deny|https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-apple-darwin.tar.gz|248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6"
      "cargo-audit|https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-apple-darwin-v0.22.2.tgz|847831323de932155b226ab60ee4a180e13e5d007a019f0d4b7b4d89a6de2ab2"
    )
    ;;
  macos-aarch64)
    tools+=(
      "cargo-deny|https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-aarch64-apple-darwin.tar.gz|fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3"
      "cargo-audit|https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-aarch64-apple-darwin-v0.22.2.tgz|ec7ca4263769593df4d909be85b94a6b79efa2897be5d2bb8ebd516e823175af"
    )
    ;;
  windows-x86_64)
    tools+=(
      "cargo-deny|https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-pc-windows-msvc.tar.gz|975a22143262fd27476d19ee00c7af67978426e40e1dee94eed6bbade1cf87dc"
      "cargo-audit|https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-pc-windows-msvc-v0.22.2.zip|0a7316540862c13d954f648917ceacca593747baed6eec180fafa590be2710ab"
    )
    ;;
  *)
    echo "install-supply-chain-tools: no pin for ${platform_os}-${platform_arch}" >&2
    exit 1
    ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

for entry in "${tools[@]}"; do
  name=${entry%%|*}
  rest=${entry#*|}
  url=${rest%%|*}
  expect=${rest##*|}
  archive="$work/$(basename "$url")"
  curl -fsSL -o "$archive" "$url"
  got=$(sha256_file "$archive")
  got_lc=$(printf '%s' "$got" | tr '[:upper:]' '[:lower:]')
  if [[ "$got_lc" != "$expect" ]]; then
    echo "install-supply-chain-tools: checksum mismatch for $name" >&2
    echo "  expected $expect" >&2
    echo "  got      $got_lc" >&2
    exit 1
  fi
  extract="$work/$name"
  mkdir -p "$extract"
  case "$archive" in
    *.zip)
      if command -v unzip >/dev/null 2>&1; then
        unzip -q "$archive" -d "$extract"
      else
        powershell.exe -NoProfile -Command "Expand-Archive -LiteralPath '$archive' -DestinationPath '$extract'"
      fi
      ;;
    *)
      tar -xzf "$archive" -C "$extract"
      ;;
  esac
  bin=$(find "$extract" -type f \( -name "$name" -o -name "$name.exe" \) | head -n 1)
  if [[ -z "$bin" ]]; then
    echo "install-supply-chain-tools: $name binary not found in archive" >&2
    exit 1
  fi
  cp "$bin" "$dest/"
  chmod +x "$dest/$(basename "$bin")"
  echo "installed $dest/$(basename "$bin")"
done
