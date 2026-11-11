#!/usr/bin/env bash
# Cross-compile tv-patchd (links the TV's libssl, libsqlite3 and libcurl via tv-tizen).
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
"$ROOT/toolchain/cross-build.sh" gnueabi build --locked --release -p tv-patchd

mkdir -p "$SCRIPT_DIR/out"
install -m 0755 "$ROOT/target/cross-gnueabi/armv7-unknown-linux-gnueabi/release/tv-patchd" \
    "$SCRIPT_DIR/out/tv-patchd"
file "$SCRIPT_DIR/out/tv-patchd"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tv-patchd" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tv-patchd"
