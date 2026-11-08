#!/usr/bin/env bash
# Cross-compile the static ARMv7 root-role client.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=rust:1.85.0-slim-bookworm@sha256:1829c432be4a592f3021501334d3fcca24f238432b13306a4e62669dec538e52
TARGET=armv7-unknown-linux-musleabihf

mkdir -p "$SCRIPT_DIR/out"
docker run --rm \
    -v "$SCRIPT_DIR:/src" -w /src \
    -e CARGO_TARGET_DIR=/src/target/cross \
    "$IMAGE" sh -eu -c '
        rustup target add "$1"
        cargo build --release --target "$1"
        cp "target/cross/$1/release/tvauth-cli" out/tvauth-cli
        chmod 0755 out/tvauth-cli
    ' sh "$TARGET"

file "$SCRIPT_DIR/out/tvauth-cli"
printf 'Size: %s bytes\n' "$(wc -c < "$SCRIPT_DIR/out/tvauth-cli" | tr -d ' ')"
shasum -a 256 "$SCRIPT_DIR/out/tvauth-cli"
