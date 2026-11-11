#!/usr/bin/env bash
# Cross-compile a static ARMv7 musl tv-authd; only the final --help smoke test
# runs under QEMU.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
"$ROOT/toolchain/cross-build.sh" musl build --locked --release -p tv-authd

mkdir -p "$SCRIPT_DIR/out"
install -m 0755 "$ROOT/target/cross-musl/armv7-unknown-linux-musleabihf/release/tv-authd" \
    "$SCRIPT_DIR/out/tv-authd"
file "$SCRIPT_DIR/out/tv-authd"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tv-authd" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tv-authd"
docker run --rm --platform linux/arm/v7 \
    -v "$SCRIPT_DIR/out:/out:ro" alpine:3.20 /out/tv-authd --help >/dev/null
printf 'ARMv7 emulated --help: OK\n'
