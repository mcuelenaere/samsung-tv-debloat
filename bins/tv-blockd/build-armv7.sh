#!/usr/bin/env bash
# Cross-compile tv-blockd (links the TV's vconf and libcurl via tv-tizen).
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
"$ROOT/cross-build.sh" gnueabi build --locked --release -p tv-blockd

mkdir -p "$SCRIPT_DIR/out"
install -m 0755 "$ROOT/target/cross-gnueabi/armv7-unknown-linux-gnueabi/release/tv-blockd" \
    "$SCRIPT_DIR/out/tv-blockd"
file "$SCRIPT_DIR/out/tv-blockd"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tv-blockd" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tv-blockd"
