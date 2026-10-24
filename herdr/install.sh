#!/usr/bin/env bash
# herdr's `[[build]]` step: put this platform's release binary in bin/. `plugin link` skips
# it, so a local checkout needs `bin/jumar` built by hand.
set -euo pipefail

REPO="joshbarrington/jumar"

# Build commands may run without herdr's runtime env, so find the root from here.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$ROOT/bin"

# The release tag matches the manifest version, so a checkout pulls its own release.
VERSION="$(grep -m1 '^version' "$ROOT/herdr-plugin.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
TAG="v${VERSION}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)                target="aarch64-apple-darwin" ;;
  Darwin-x86_64)               target="x86_64-apple-darwin" ;;
  Linux-aarch64 | Linux-arm64) target="aarch64-unknown-linux-musl" ;;
  Linux-x86_64)                target="x86_64-unknown-linux-musl" ;;
  *)
    echo "jumar: no prebuilt binary for $(uname -s)-$(uname -m); run 'cargo build --release' and copy target/release/jumar to bin/" >&2
    exit 1
    ;;
esac

archive="jumar-${target}.tar.xz"
base="https://github.com/${REPO}/releases/download/${TAG}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# GitHub's CDN can 404 a fresh release's assets for a few minutes.
download() {
  for _ in 1 2 3 4 5 6; do
    curl -fsSL "$1" -o "$2" && return
    sleep 3
  done
  return 1
}

echo "jumar: downloading $archive ($TAG)"
download "$base/$archive" "$tmp/$archive"
download "$base/$archive.sha256" "$tmp/$archive.sha256"

expected="$(awk '{print $1}' "$tmp/$archive.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
fi
if [ "$expected" != "$actual" ]; then
  echo "jumar: checksum mismatch (expected $expected, got $actual)" >&2
  exit 1
fi

tar -xJf "$tmp/$archive" -C "$tmp"
binary="$(find "$tmp" -type f -name jumar | head -n 1)"
if [ -z "$binary" ]; then
  echo "jumar: $archive has no jumar binary" >&2
  exit 1
fi
mkdir -p "$BIN_DIR"
install -m 0755 "$binary" "$BIN_DIR/jumar"
echo "jumar: installed $BIN_DIR/jumar"
