#!/usr/bin/env bash
# Cross-compile the static ARMv7 root-role client.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
"$ROOT/toolchain/cross-build.sh" musl build --locked --release -p tvauth-cli

mkdir -p "$SCRIPT_DIR/out"
install -m 0755 "$ROOT/target/cross-musl/armv7-unknown-linux-musleabihf/release/tvauth-cli" \
    "$SCRIPT_DIR/out/tvauth-cli"
file "$SCRIPT_DIR/out/tvauth-cli"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tvauth-cli" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tvauth-cli"
