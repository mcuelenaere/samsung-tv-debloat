#!/usr/bin/env bash
# Cross-compile tv-auth-popupd against the TV's EFL stack.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
"$ROOT/toolchain/cross-build.sh" gnueabi build --locked --release -p tv-auth-popupd

mkdir -p "$SCRIPT_DIR/out"
install -m 0755 "$ROOT/target/cross-gnueabi/armv7-unknown-linux-gnueabi/release/tv-auth-popupd" \
    "$SCRIPT_DIR/out/tv-auth-popupd"
file "$SCRIPT_DIR/out/tv-auth-popupd"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tv-auth-popupd" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tv-auth-popupd"
