#!/usr/bin/env bash
#
# Build the polyptych release binary, then package it as a tarball, .deb and
# .rpm for this machine's architecture.
#
#   packaging/build-packages.sh
#
# Output lands in packaging/dist/. Requires cargo and `nfpm` on PATH (a single
# static Go binary — https://nfpm.goreleaser.com/install/). The version comes
# from Cargo.toml.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
cd "$REPO_DIR"

DIST="packaging/dist"
ARCH="$(uname -m)"
POLYPTYCH_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
export POLYPTYCH_VERSION
# nfpm takes Go architecture names and maps them per package format.
case "$ARCH" in
  x86_64) export NFPM_ARCH=amd64 ;;
  aarch64) export NFPM_ARCH=arm64 ;;
  *)
    echo "error: unsupported architecture $ARCH" >&2
    exit 1
    ;;
esac

if ! command -v nfpm >/dev/null 2>&1; then
  echo "error: nfpm not found on PATH — see https://nfpm.goreleaser.com/install/" >&2
  exit 1
fi

echo "==> Building polyptych $POLYPTYCH_VERSION ($ARCH)"
cargo build --release --locked
rm -rf "$DIST"
mkdir -p "$DIST"

echo "==> Building tarball"
name="polyptych-$POLYPTYCH_VERSION-$ARCH-linux"
mkdir "$DIST/$name"
cp target/release/polyptych assets/polyptych.svg packaging/polyptych.desktop README.md LICENSE "$DIST/$name/"
tar -C "$DIST" -czf "$DIST/$name.tar.gz" "$name"
rm -r "${DIST:?}/$name"

echo "==> Building .deb"
nfpm package -f packaging/nfpm.yaml -p deb -t "$DIST"

echo "==> Building .rpm"
nfpm package -f packaging/nfpm.yaml -p rpm -t "$DIST"

echo
echo "Done. Packages in $DIST/:"
ls -1 "$DIST"
