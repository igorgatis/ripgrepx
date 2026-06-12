#!/bin/sh
# rgx installer — downloads the prebuilt, self-contained binary from GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/igorgatis/ripgrepx/main/install.sh | sh
#
# Env overrides:
#   RGX_VERSION  release tag to install (default: latest, e.g. v0.2.0)
#   RGX_TARGET   Rust target triple (default: autodetected from uname)
#   RGX_USE_MUSL set to 1 on Linux to fetch the static musl build
#   BIN_DIR      install directory (default: ~/.local/bin)
#
# Windows isn't covered here — use `npm i -g ripgrepx`, `pipx install ripgrepx`,
# `cargo install ripgrepx`, or the release .zip.
set -eu

REPO="igorgatis/ripgrepx"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

say() { printf 'rgx-install: %s\n' "$1" >&2; }
die() { say "error: $1"; exit 1; }

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1"; }
  download() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO- "$1"; }
  download() { wget -qO "$2" "$1"; }
else
  die "need curl or wget"
fi
command -v tar >/dev/null 2>&1 || die "need tar"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    sys="unknown-linux-gnu"
    [ "${RGX_USE_MUSL:-}" = "1" ] && sys="unknown-linux-musl"
    ;;
  Darwin) sys="apple-darwin" ;;
  *) die "unsupported OS '$os' — on Windows use npm/pipx/cargo or the release .zip" ;;
esac
case "$arch" in
  x86_64 | amd64) cpu="x86_64" ;;
  arm64 | aarch64) cpu="aarch64" ;;
  *) die "unsupported architecture '$arch'" ;;
esac
target="${RGX_TARGET:-${cpu}-${sys}}"

ver="${RGX_VERSION:-}"
if [ -z "$ver" ]; then
  ver="$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -o '"tag_name"[ ]*:[ ]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/')"
  [ -n "$ver" ] || die "could not resolve the latest version (set RGX_VERSION)"
fi

asset="rgx-${ver}-${target}.tar.gz"
url="https://github.com/$REPO/releases/download/$ver/$asset"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "downloading $asset"
download "$url" "$tmp/$asset" || die "download failed: $url"
tar -xzf "$tmp/$asset" -C "$tmp" || die "could not extract $asset"
[ -f "$tmp/rgx" ] || die "archive did not contain rgx"

mkdir -p "$BIN_DIR"
install -m 0755 "$tmp/rgx" "$BIN_DIR/rgx" 2>/dev/null || {
  cp "$tmp/rgx" "$BIN_DIR/rgx" && chmod 0755 "$BIN_DIR/rgx"
}
say "installed rgx $ver -> $BIN_DIR/rgx"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "note: $BIN_DIR is not on your PATH; add it, e.g. export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
"$BIN_DIR/rgx" --version 2>/dev/null || true
